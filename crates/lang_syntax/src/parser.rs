//! Recursive-descent parser for declarations and statements, precedence climbing for
//! expressions (precedence table in docs/01-language-design.md, "Grammar sketch").
//!
//! Error recovery: a failed statement or declaration is reported once, then the parser skips to
//! the end of the line and over any indented block that follows. Because blocks are delimited by
//! layout tokens, one bad line rarely disturbs the rest of the file.

use crate::ast::*;
use crate::lexer::lex_fragment;
use crate::token::{FPiece, Token, TokenKind};
use lang_diag::{Diagnostic, Diagnostics, Span, codes};

pub fn parse(tokens: &[Token], diags: &mut Diagnostics) -> Module {
    assert!(matches!(tokens.last(), Some(Token { kind: TokenKind::Eof, .. })), "token stream must end with Eof");
    let mut parser = Parser { tokens, pos: 0, line_start: 0, diags };
    parser.module()
}

/// Marker for "an error was reported; recover".
struct Reported;

type PResult<T> = Result<T, Reported>;

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
    /// Byte offset where the current logical line starts.
    line_start: u32,
    diags: &'a mut Diagnostics,
}

const PREC_OR: u8 = 1;
const PREC_AND: u8 = 2;
const PREC_NOT: u8 = 3;
const PREC_COMPARE: u8 = 4;
const PREC_UNARY: u8 = 11;
const PREC_POW: u8 = 12;

impl Parser<'_> {
    // ---- token access -------------------------------------------------------------------

    fn peek(&self) -> &TokenKind {
        &self.tokens[self.pos].kind
    }

    fn peek_nth(&self, n: usize) -> &TokenKind {
        &self.tokens[(self.pos + n).min(self.tokens.len() - 1)].kind
    }

    fn span(&self) -> Span {
        self.tokens[self.pos].span
    }

    fn prev_span(&self) -> Span {
        self.tokens[self.pos.saturating_sub(1)].span
    }

    fn bump(&mut self) -> Token {
        let token = self.tokens[self.pos].clone();
        if token.kind != TokenKind::Eof {
            self.pos += 1;
        }
        if matches!(token.kind, TokenKind::Newline | TokenKind::Indent | TokenKind::Dedent) {
            self.line_start = self.tokens[self.pos].span.lo;
        }
        token
    }

    fn at(&self, kind: &TokenKind) -> bool {
        self.peek() == kind
    }

    fn eat(&mut self, kind: &TokenKind) -> bool {
        let found = self.at(kind);
        if found {
            self.bump();
        }
        found
    }

    fn at_line_end(&self) -> bool {
        matches!(self.peek(), TokenKind::Newline | TokenKind::Dedent | TokenKind::Eof)
    }

    // ---- errors and recovery ------------------------------------------------------------

    /// Where to point when the current token is wrong. Layout tokens have no useful position
    /// of their own, so errors about them point just past the previous token.
    fn error_span(&self) -> Span {
        match self.peek() {
            TokenKind::Newline | TokenKind::Indent | TokenKind::Dedent | TokenKind::Eof if self.pos > 0 => {
                self.prev_span().end()
            }
            _ => self.span(),
        }
    }

    fn unexpected(&mut self, expected: &str) -> Reported {
        let found = self.peek().describe();
        let span = self.error_span();
        // An earlier error on this line (often from the lexer, such as an unterminated string)
        // is the real problem; this one is a consequence of it.
        if self.diags.has_error_in(span.file, self.line_start, span.hi) {
            return Reported;
        }
        self.diags.push(Diagnostic::error(
            codes::UNEXPECTED_TOKEN,
            format!("{expected}, found {found}"),
            span,
            format!("unexpected {found}"),
        ));
        Reported
    }

    fn not_supported(&mut self, what: &str, span: Span) -> Reported {
        self.diags.push(Diagnostic::error(
            codes::NOT_SUPPORTED_SYNTAX,
            format!("{what} not supported yet"),
            span,
            "not supported yet",
        ));
        Reported
    }

    fn error(&mut self, message: impl Into<String>, span: Span, label: &str, help: Option<&str>) -> Reported {
        let mut diagnostic = Diagnostic::error(codes::UNEXPECTED_TOKEN, message, span, label);
        if let Some(help) = help {
            diagnostic = diagnostic.with_help(help);
        }
        self.diags.push(diagnostic);
        Reported
    }

    fn expect(&mut self, kind: TokenKind, context: &str) -> PResult<Span> {
        if self.at(&kind) {
            Ok(self.bump().span)
        } else {
            Err(self.unexpected(&format!("expected {} {context}", kind.describe())))
        }
    }

    fn expect_name(&mut self, what: &str) -> PResult<Ident> {
        if let TokenKind::Name(name) = self.peek() {
            let name = name.clone();
            let span = self.bump().span;
            Ok(Ident { name, span })
        } else {
            Err(self.unexpected(&format!("expected {what}")))
        }
    }

    /// Skip the rest of the current line, and the indented block after it if there is one.
    fn skip_statement(&mut self) {
        while !self.at_line_end() {
            self.bump();
        }
        self.eat(&TokenKind::Newline);
        if self.at(&TokenKind::Indent) {
            self.skip_block();
        }
    }

    /// Skip a whole indented block, starting at its `Indent`.
    fn skip_block(&mut self) {
        let mut depth = 0usize;
        loop {
            match self.bump().kind {
                TokenKind::Indent => depth += 1,
                TokenKind::Dedent => {
                    depth -= 1;
                    if depth == 0 {
                        return;
                    }
                }
                TokenKind::Eof => return,
                _ => {}
            }
        }
    }

    fn unexpected_indent(&mut self) {
        let span = self.span();
        self.diags.push(
            Diagnostic::error(
                codes::UNEXPECTED_TOKEN,
                "unexpected indentation",
                span,
                "this line is indented further than the one before it",
            )
            .with_help("only the line after one ending in `:` starts an indented block"),
        );
        self.skip_block();
    }

    fn end_of_statement(&mut self) -> PResult<()> {
        match self.peek() {
            TokenKind::Newline => {
                self.bump();
                Ok(())
            }
            TokenKind::Dedent | TokenKind::Eof => Ok(()),
            _ => Err(self.unexpected("expected end of line")),
        }
    }

    /// After a block's `:`, expect a line break and an indented block start.
    fn block_start(&mut self) -> PResult<()> {
        if !self.at(&TokenKind::Newline) {
            let found = self.peek().describe();
            let span = self.error_span();
            return Err(self.error(
                format!("expected a new line after `:`, found {found}"),
                span,
                "expected a line break",
                Some("put the body on the following lines, indented"),
            ));
        }
        self.bump();
        if !self.at(&TokenKind::Indent) {
            let span = self.error_span();
            return Err(self.error(
                "expected an indented block",
                span,
                "the block should start on the next line",
                Some("indent the lines that belong to this block"),
            ));
        }
        self.bump();
        Ok(())
    }

    // ---- declarations -------------------------------------------------------------------

    fn module(&mut self) -> Module {
        let start = Span { lo: 0, hi: 0, ..self.span() };
        let mut items = Vec::new();
        loop {
            match self.peek().clone() {
                TokenKind::Eof => break,
                TokenKind::Newline | TokenKind::Dedent => {
                    self.bump();
                }
                TokenKind::Indent => self.unexpected_indent(),
                TokenKind::Def
                | TokenKind::Pub
                | TokenKind::Priv
                | TokenKind::Struct
                | TokenKind::Enum
                | TokenKind::Trait
                | TokenKind::Impl
                | TokenKind::Const
                | TokenKind::At => match self.item() {
                    Ok(item) => items.push(item),
                    Err(Reported) => self.skip_statement(),
                },
                TokenKind::Name(word)
                    if word == "type"
                        && matches!(self.peek_nth(1), TokenKind::Name(_))
                        && *self.peek_nth(2) == TokenKind::Assign =>
                {
                    match self.type_alias(Visibility::Default) {
                        Ok(alias) => items.push(Item::TypeAlias(alias)),
                        Err(Reported) => self.skip_statement(),
                    }
                }
                TokenKind::Import | TokenKind::From => match self.import_decl() {
                    Ok(import) => items.push(Item::Import(import)),
                    Err(Reported) => self.skip_statement(),
                },
                TokenKind::Name(name) if name == "class" => {
                    let span = self.span();
                    self.error(
                        "there are no classes",
                        span,
                        "not a keyword",
                        Some("define data with `struct` and behaviour with `impl` blocks and traits"),
                    );
                    self.skip_statement();
                }
                TokenKind::Str(_) if matches!(self.peek_nth(1), TokenKind::Newline | TokenKind::Eof) => {
                    // A module docstring.
                    self.bump();
                    self.eat(&TokenKind::Newline);
                }
                _ => {
                    let found = self.peek().describe();
                    let span = self.error_span();
                    self.error(
                        format!("expected a declaration, found {found}"),
                        span,
                        "statements are not allowed here",
                        Some("statements must be inside a function; the program starts in `def main():`"),
                    );
                    self.skip_statement();
                }
            }
        }
        Module { items, start }
    }

    /// `type Name = T`, after any `pub` or `priv`.
    fn type_alias(&mut self, visibility: Visibility) -> PResult<TypeAliasDecl> {
        self.bump();
        let name = self.expect_name("a name for the type")?;
        self.expect(TokenKind::Assign, "after the type's name")?;
        let ty = self.type_expr()?;
        self.end_of_statement()?;
        Ok(TypeAliasDecl { visibility, name, ty })
    }

    /// A dotted module name: `a.b.c`.
    fn module_path(&mut self) -> PResult<Vec<Ident>> {
        let mut path = vec![self.expect_name("a module name")?];
        while self.eat(&TokenKind::Dot) {
            path.push(self.expect_name("a module name after `.`")?);
        }
        Ok(path)
    }

    fn import_decl(&mut self) -> PResult<ImportDecl> {
        let start = self.span();
        if self.eat(&TokenKind::Import) {
            let path = self.module_path()?;
            let alias = if self.eat(&TokenKind::As) { Some(self.expect_name("a name after `as`")?) } else { None };
            let span = start.to(self.prev_span());
            self.end_of_statement()?;
            return Ok(ImportDecl { path, alias, names: None, span });
        }
        self.expect(TokenKind::From, "")?;
        let path = self.module_path()?;
        self.expect(TokenKind::Import, "after the module name")?;
        let parenthesised = self.eat(&TokenKind::LParen);
        let mut names = Vec::new();
        loop {
            if self.at(&TokenKind::Star) {
                let span = self.span();
                return Err(self.error(
                    "`from ... import *` is not supported",
                    span,
                    "name the imports",
                    Some("list the names you use, or import the module and write `module.name`"),
                ));
            }
            let name = self.expect_name("a name to import")?;
            let alias = if self.eat(&TokenKind::As) { Some(self.expect_name("a name after `as`")?) } else { None };
            names.push((name, alias));
            if !self.eat(&TokenKind::Comma) || (parenthesised && self.at(&TokenKind::RParen)) {
                break;
            }
        }
        if parenthesised {
            self.expect(TokenKind::RParen, "to close the list of names")?;
        }
        let span = start.to(self.prev_span());
        self.end_of_statement()?;
        Ok(ImportDecl { path, alias: None, names: Some(names), span })
    }

    fn visibility(&mut self) -> Visibility {
        if self.eat(&TokenKind::Pub) {
            Visibility::Pub
        } else if self.eat(&TokenKind::Priv) {
            Visibility::Priv
        } else {
            Visibility::Default
        }
    }

    /// `@name` and `@name(a, b)` lines before a declaration.
    fn annotations(&mut self) -> PResult<Vec<Annotation>> {
        let mut annotations = Vec::new();
        while self.at(&TokenKind::At) {
            let start = self.bump().span;
            let mut name = self.expect_name("an annotation name after `@`")?;
            while self.eat(&TokenKind::Dot) {
                let part = self.expect_name("a name after `.`")?;
                name = Ident { name: format!("{}.{}", name.name, part.name), span: name.span.to(part.span) };
            }
            let mut args = Vec::new();
            if self.eat(&TokenKind::LParen) {
                while !self.at(&TokenKind::RParen) {
                    args.push(self.annotation_arg()?);
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(TokenKind::RParen, "to close the annotation's arguments")?;
            }
            let span = start.to(self.prev_span());
            if !self.at(&TokenKind::Newline) {
                return Err(self.unexpected("expected the annotation to end its line"));
            }
            self.bump();
            annotations.push(Annotation { name, args, span });
        }
        Ok(annotations)
    }

    /// `name`, `"text"`, or `keyword=value` inside an annotation's parentheses.
    fn annotation_arg(&mut self) -> PResult<AnnotationArg> {
        let start = self.span();
        let keyword = match (self.peek().clone(), self.peek_nth(1).clone()) {
            (TokenKind::Name(_), TokenKind::Assign) => {
                let keyword = self.expect_name("a name")?;
                self.bump();
                Some(keyword)
            }
            _ => None,
        };
        let value = match self.peek().clone() {
            TokenKind::Str(text) => {
                self.bump();
                AnnotationValue::Str(text)
            }
            TokenKind::Name(_) => AnnotationValue::Name(self.expect_name("a name")?),
            TokenKind::Int(value) => {
                self.bump();
                AnnotationValue::Int(i64::try_from(value).unwrap_or(i64::MAX))
            }
            _ => return Err(self.unexpected("expected a name, a string or a number")),
        };
        Ok(AnnotationArg { keyword, value, span: start.to(self.prev_span()) })
    }

    fn item(&mut self) -> PResult<Item> {
        let annotations = self.annotations()?;
        let vis_span = self.span();
        let visibility = self.visibility();
        if let Some(first) = annotations.first()
            && !matches!(self.peek(), TokenKind::Struct | TokenKind::Enum | TokenKind::Def)
        {
            let span = first.span;
            return Err(self.error(
                "annotations are only allowed on `def`, `struct` and `enum` declarations",
                span,
                "not allowed here",
                None,
            ));
        }
        if matches!(self.peek(), TokenKind::Name(word) if word == "type") && annotations.is_empty() {
            return Ok(Item::TypeAlias(self.type_alias(visibility)?));
        }
        match self.peek() {
            TokenKind::Def => Ok(Item::Function(self.function_with(annotations, visibility, false)?)),
            TokenKind::Struct => Ok(Item::Struct(self.struct_decl(annotations, visibility)?)),
            TokenKind::Enum => Ok(Item::Enum(self.enum_decl(annotations, visibility)?)),
            TokenKind::Trait => Ok(Item::Trait(self.trait_decl(visibility)?)),
            TokenKind::Const => Ok(Item::Const(self.const_decl(visibility)?)),
            TokenKind::Impl => {
                if visibility != Visibility::Default {
                    return Err(self.error(
                        "`impl` blocks cannot be `pub` or `priv`",
                        vis_span,
                        "remove this",
                        Some("mark the individual methods instead"),
                    ));
                }
                Ok(Item::Impl(self.impl_decl()?))
            }
            _ => Err(self.unexpected("expected `def`, `struct`, `enum`, `trait` or `const`")),
        }
    }

    /// A function. Inside a trait (`in_trait`), the body may be left out to declare a required
    /// method: `def area(self) -> float`.
    fn function(&mut self, visibility: Visibility, in_trait: bool) -> PResult<Function> {
        self.function_with(Vec::new(), visibility, in_trait)
    }

    fn function_with(
        &mut self,
        annotations: Vec<Annotation>,
        visibility: Visibility,
        in_trait: bool,
    ) -> PResult<Function> {
        let bodiless_allowed = in_trait || annotations.iter().any(|a| a.name.name == "extern");
        self.expect(TokenKind::Def, "")?;
        let name = self.expect_name("a function name")?;
        let generics = self.generic_params()?;
        self.expect(TokenKind::LParen, "after the function name")?;

        let mut self_param = None;
        let mut params = Vec::new();
        while !self.at(&TokenKind::RParen) {
            if self.at(&TokenKind::SelfKw) {
                let span = self.bump().span;
                if !params.is_empty() || self_param.is_some() {
                    return Err(self.error("`self` must be the first parameter", span, "move this first", None));
                }
                if self.at(&TokenKind::Colon) {
                    return Err(self.error("`self` takes no type hint", self.span(), "remove the type hint", None));
                }
                self_param = Some(span);
            } else {
                let name = self.expect_name("a parameter name")?;
                let ty = if self.eat(&TokenKind::Colon) { Some(self.type_expr()?) } else { None };
                let default = if self.eat(&TokenKind::Assign) { Some(self.expr()?) } else { None };
                params.push(Param { name, ty, default });
            }
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RParen, "to close the parameter list")?;
        let ret = if self.eat(&TokenKind::Arrow) { Some(self.type_expr()?) } else { None };
        if bodiless_allowed && self.at_line_end() {
            self.end_of_statement()?;
            return Ok(Function {
                annotations,
                visibility,
                name,
                generics,
                self_param,
                params,
                ret,
                body: Vec::new(),
                has_body: false,
            });
        }
        if !self.at(&TokenKind::Colon) {
            let found = self.peek().describe();
            let span = self.error_span();
            return Err(self.error(
                format!("expected `:` after the function signature, found {found}"),
                span,
                "expected `:`",
                Some("a function header ends with `:`, and its body follows on the next lines, indented"),
            ));
        }
        self.bump();
        let body = self.block()?;
        Ok(Function { annotations, visibility, name, generics, self_param, params, ret, body, has_body: true })
    }

    /// `[T, U: Ord + Display]` after a declaration's name; empty when there is none.
    fn generic_params(&mut self) -> PResult<Vec<GenericParam>> {
        let mut params = Vec::new();
        if !self.eat(&TokenKind::LBracket) {
            return Ok(params);
        }
        while !self.at(&TokenKind::RBracket) {
            let name = self.expect_name("a type parameter name")?;
            let mut bounds = Vec::new();
            if self.eat(&TokenKind::Colon) {
                bounds.push(self.expect_name("a trait name")?);
                while self.eat(&TokenKind::Plus) {
                    bounds.push(self.expect_name("a trait name")?);
                }
            }
            params.push(GenericParam { name, bounds });
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RBracket, "to close the type parameters")?;
        Ok(params)
    }

    fn struct_decl(&mut self, annotations: Vec<Annotation>, visibility: Visibility) -> PResult<StructDecl> {
        self.bump();
        let name = self.expect_name("a struct name")?;
        let generics = self.generic_params()?;
        if self.at(&TokenKind::LParen) {
            let span = self.span();
            return Err(self.error(
                "structs cannot inherit from other types",
                span,
                "not allowed",
                Some("share behaviour through traits, or include the other struct as a field"),
            ));
        }
        self.expect(TokenKind::Colon, "after the struct name")?;
        self.block_start()?;
        let mut fields = Vec::new();
        loop {
            match self.peek().clone() {
                TokenKind::Dedent => {
                    self.bump();
                    break;
                }
                TokenKind::Eof => break,
                TokenKind::Newline => {
                    self.bump();
                }
                TokenKind::Str(_) | TokenKind::Pass => {
                    // A docstring, or `pass` in a struct with no fields.
                    self.bump();
                    if self.end_of_statement().is_err() {
                        self.skip_statement();
                    }
                }
                TokenKind::Def => {
                    let span = self.span();
                    self.error(
                        "methods are defined in an `impl` block, not inside the struct",
                        span,
                        "not allowed here",
                        Some(&format!("add `impl {}:` after the struct and put the method there", name.name)),
                    );
                    self.skip_statement();
                }
                _ => match self.field() {
                    Ok(field) => fields.push(field),
                    Err(Reported) => self.skip_statement(),
                },
            }
        }
        Ok(StructDecl { annotations, visibility, name, generics, fields })
    }

    fn field(&mut self) -> PResult<FieldDecl> {
        let annotations = self.annotations()?;
        let visibility = self.visibility();
        let name = self.expect_name("a field name")?;
        if !self.at(&TokenKind::Colon) {
            let span = self.error_span();
            return Err(self.error(
                format!("field `{}` needs a type", name.name),
                span,
                "expected `:` and a type",
                Some(&format!("write it as `{}: int`, with the field's type", name.name)),
            ));
        }
        self.bump();
        let ty = self.type_expr()?;
        let default = if self.eat(&TokenKind::Assign) { Some(self.expr()?) } else { None };
        self.end_of_statement()?;
        Ok(FieldDecl { annotations, visibility, name, ty, default })
    }

    fn enum_decl(&mut self, annotations: Vec<Annotation>, visibility: Visibility) -> PResult<EnumDecl> {
        self.bump();
        let name = self.expect_name("an enum name")?;
        let generics = self.generic_params()?;
        self.expect(TokenKind::Colon, "after the enum name")?;
        self.block_start()?;
        let mut variants = Vec::new();
        loop {
            match self.peek() {
                TokenKind::Dedent => {
                    self.bump();
                    break;
                }
                TokenKind::Eof => break,
                TokenKind::Newline => {
                    self.bump();
                }
                TokenKind::Str(_) | TokenKind::Pass => {
                    self.bump();
                    if self.end_of_statement().is_err() {
                        self.skip_statement();
                    }
                }
                TokenKind::Def => {
                    let span = self.span();
                    self.error(
                        "methods are defined in an `impl` block, not inside the enum",
                        span,
                        "not allowed here",
                        Some(&format!("add `impl {}:` after the enum and put the method there", name.name)),
                    );
                    self.skip_statement();
                }
                _ => match self.variant() {
                    Ok(variant) => variants.push(variant),
                    Err(Reported) => self.skip_statement(),
                },
            }
        }
        Ok(EnumDecl { annotations, visibility, name, generics, variants })
    }

    /// `Name` or `Name(field: type, ...)`.
    fn variant(&mut self) -> PResult<VariantDecl> {
        let name = self.expect_name("a variant name")?;
        let mut fields = Vec::new();
        if self.eat(&TokenKind::LParen) {
            while !self.at(&TokenKind::RParen) {
                let field_name = self.expect_name("a field name")?;
                if !self.at(&TokenKind::Colon) {
                    let span = self.error_span();
                    return Err(self.error(
                        format!("field `{}` needs a type", field_name.name),
                        span,
                        "expected `:` and a type",
                        Some(&format!("write it as `{}: str`, with the field's type", field_name.name)),
                    ));
                }
                self.bump();
                let ty = self.type_expr()?;
                if self.at(&TokenKind::Assign) {
                    let span = self.span();
                    return Err(self.not_supported("default values for variant fields are", span));
                }
                fields.push(FieldDecl {
                    annotations: Vec::new(),
                    visibility: Visibility::Default,
                    name: field_name,
                    ty,
                    default: None,
                });
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::RParen, "to close the variant's fields")?;
        }
        self.end_of_statement()?;
        Ok(VariantDecl { name, fields })
    }

    fn trait_decl(&mut self, visibility: Visibility) -> PResult<TraitDecl> {
        self.bump();
        let name = self.expect_name("a trait name")?;
        if self.at(&TokenKind::LBracket) {
            let span = self.span();
            return Err(self.not_supported("generic traits are", span));
        }
        let mut supertraits = Vec::new();
        if self.eat(&TokenKind::LParen) {
            while !self.at(&TokenKind::RParen) {
                supertraits.push(self.expect_name("a trait name")?);
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::RParen, "to close the list of required traits")?;
        }
        self.expect(TokenKind::Colon, "after the trait name")?;
        self.block_start()?;
        let mut methods = Vec::new();
        loop {
            match self.peek() {
                TokenKind::Dedent => {
                    self.bump();
                    break;
                }
                TokenKind::Eof => break,
                TokenKind::Newline => {
                    self.bump();
                }
                TokenKind::Str(_) | TokenKind::Pass => {
                    self.bump();
                    if self.end_of_statement().is_err() {
                        self.skip_statement();
                    }
                }
                TokenKind::Def | TokenKind::Pub | TokenKind::Priv => {
                    let visibility = self.visibility();
                    match self.function(visibility, true) {
                        Ok(method) => methods.push(method),
                        Err(Reported) => self.skip_statement(),
                    }
                }
                _ => {
                    self.unexpected("expected a method (`def`)");
                    self.skip_statement();
                }
            }
        }
        Ok(TraitDecl { visibility, name, supertraits, methods })
    }

    fn impl_decl(&mut self) -> PResult<ImplDecl> {
        self.bump();
        let generics = self.generic_params()?;
        let first = self.expect_name("a type or trait name after `impl`")?;
        let mut target_args = self.type_args()?;
        let (trait_name, target) = if self.eat(&TokenKind::For) {
            if !target_args.is_empty() {
                let span = first.span;
                return Err(self.not_supported("generic traits are", span));
            }
            let target = self.expect_name("a type name after `for`")?;
            target_args = self.type_args()?;
            (Some(first), target)
        } else {
            (None, first)
        };
        self.expect(TokenKind::Colon, "after the type name")?;
        self.block_start()?;
        let mut methods = Vec::new();
        loop {
            match self.peek() {
                TokenKind::Dedent => {
                    self.bump();
                    break;
                }
                TokenKind::Eof => break,
                TokenKind::Newline => {
                    self.bump();
                }
                TokenKind::Str(_) | TokenKind::Pass => {
                    self.bump();
                    if self.end_of_statement().is_err() {
                        self.skip_statement();
                    }
                }
                TokenKind::Def | TokenKind::Pub | TokenKind::Priv | TokenKind::At => {
                    let method = self.annotations().and_then(|annotations| {
                        let visibility = self.visibility();
                        self.function_with(annotations, visibility, false)
                    });
                    match method {
                        Ok(method) => methods.push(method),
                        Err(Reported) => self.skip_statement(),
                    }
                }
                _ => {
                    self.unexpected("expected a method (`def`)");
                    self.skip_statement();
                }
            }
        }
        Ok(ImplDecl { generics, trait_name, target, target_args, methods })
    }

    fn const_decl(&mut self, visibility: Visibility) -> PResult<ConstDecl> {
        self.bump();
        let name = self.expect_name("a constant name")?;
        let ty = if self.eat(&TokenKind::Colon) { Some(self.type_expr()?) } else { None };
        self.expect(TokenKind::Assign, "and a value for the constant")?;
        let value = self.expr()?;
        self.end_of_statement()?;
        Ok(ConstDecl { visibility, name, ty, value })
    }

    /// `[A, B]` type arguments after a type name; empty when there are none.
    fn type_args(&mut self) -> PResult<Vec<TypeExpr>> {
        let mut args = Vec::new();
        if self.eat(&TokenKind::LBracket) {
            loop {
                args.push(self.type_expr()?);
                if !self.eat(&TokenKind::Comma) || self.at(&TokenKind::RBracket) {
                    break;
                }
            }
            self.expect(TokenKind::RBracket, "to close the type arguments")?;
        }
        Ok(args)
    }

    fn type_expr(&mut self) -> PResult<TypeExpr> {
        let start = self.span();
        let mut ty = match self.peek().clone() {
            TokenKind::Def => {
                self.bump();
                self.expect(TokenKind::LParen, "after `def` in a function type")?;
                let mut params = Vec::new();
                while !self.at(&TokenKind::RParen) {
                    params.push(self.type_expr()?);
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(TokenKind::RParen, "to close the parameter types")?;
                let ret = if self.eat(&TokenKind::Arrow) { Some(Box::new(self.type_expr()?)) } else { None };
                TypeExpr { kind: TypeExprKind::Func { params, ret }, span: start.to(self.prev_span()) }
            }
            TokenKind::NoneKw => {
                self.bump();
                TypeExpr { kind: TypeExprKind::None, span: start }
            }
            TokenKind::LParen => {
                // `(def(int) -> str)?`: parentheses group a type.
                self.bump();
                let inner = self.type_expr()?;
                self.expect(TokenKind::RParen, "to close the parenthesised type")?;
                TypeExpr { kind: inner.kind, span: start.to(self.prev_span()) }
            }
            TokenKind::Name(mut name) => {
                self.bump();
                // A type from another module: `models.User`.
                while self.at(&TokenKind::Dot) && matches!(self.peek_nth(1), TokenKind::Name(_)) {
                    self.bump();
                    let part = self.expect_name("a type name")?;
                    name = format!("{name}.{}", part.name);
                }
                let mut args = Vec::new();
                if self.eat(&TokenKind::LBracket) {
                    loop {
                        // `tuple[T, ...]`: the `...` is kept as a type argument named "...".
                        if self.at(&TokenKind::Dot)
                            && *self.peek_nth(1) == TokenKind::Dot
                            && *self.peek_nth(2) == TokenKind::Dot
                        {
                            let dots = self.bump().span;
                            self.bump();
                            let last = self.bump().span;
                            args.push(TypeExpr {
                                kind: TypeExprKind::Named { name: "...".to_string(), args: Vec::new() },
                                span: dots.to(last),
                            });
                        } else {
                            args.push(self.type_expr()?);
                        }
                        if !self.eat(&TokenKind::Comma) || self.at(&TokenKind::RBracket) {
                            break;
                        }
                    }
                    self.expect(TokenKind::RBracket, "to close the type arguments")?;
                }
                TypeExpr { kind: TypeExprKind::Named { name, args }, span: start.to(self.prev_span()) }
            }
            _ => return Err(self.unexpected("expected a type")),
        };
        loop {
            if self.at(&TokenKind::Question) {
                self.bump();
            } else if self.at(&TokenKind::Pipe) && *self.peek_nth(1) == TokenKind::NoneKw {
                self.bump();
                self.bump();
            } else {
                break;
            }
            let span = ty.span.to(self.prev_span());
            ty = TypeExpr { kind: TypeExprKind::Optional(Box::new(ty)), span };
        }
        Ok(ty)
    }

    // ---- statements ---------------------------------------------------------------------

    /// `NEWLINE INDENT stmt+ DEDENT`, after the `:` has been consumed.
    fn block(&mut self) -> PResult<Vec<Stmt>> {
        self.block_start()?;
        let mut stmts = Vec::new();
        loop {
            match self.peek() {
                TokenKind::Dedent => {
                    self.bump();
                    break;
                }
                TokenKind::Eof => break,
                TokenKind::Newline => {
                    self.bump();
                }
                TokenKind::Indent => self.unexpected_indent(),
                _ => match self.statement() {
                    Ok(stmt) => stmts.push(stmt),
                    Err(Reported) => self.skip_statement(),
                },
            }
        }
        Ok(stmts)
    }

    /// `assert cond` or `assert cond, message`, which mean
    /// `if not cond: panic(message)`; the panic reports the `assert`'s location.
    fn assert_stmt(&mut self) -> PResult<Stmt> {
        let start = self.span();
        self.bump();
        let cond = self.expr()?;
        let message = if self.peek() == &TokenKind::Comma {
            self.bump();
            self.expr()?
        } else {
            Expr { kind: ExprKind::Str("assertion failed".to_string()), span: cond.span }
        };
        let span = start.to(self.prev_span());
        self.end_of_statement()?;
        let not = Expr { kind: ExprKind::Unary { op: UnaryOp::Not, operand: Box::new(cond) }, span };
        let callee = Expr { kind: ExprKind::Name("panic".to_string()), span: start };
        let call = Expr {
            kind: ExprKind::Call { callee: Box::new(callee), args: vec![Arg { name: None, value: message }] },
            span,
        };
        let fail = Stmt { kind: StmtKind::Expr(call), span };
        Ok(Stmt { kind: StmtKind::If { branches: vec![(not, vec![fail])], otherwise: None }, span })
    }

    fn statement(&mut self) -> PResult<Stmt> {
        let start = self.span();
        let kind = match self.peek().clone() {
            TokenKind::Return => {
                self.bump();
                let value = if self.at_line_end() { None } else { Some(self.expr_list()?) };
                let span = start.to(self.prev_span());
                self.end_of_statement()?;
                return Ok(Stmt { kind: StmtKind::Return(value), span });
            }
            TokenKind::If => return self.if_stmt(),
            TokenKind::While => {
                self.bump();
                let cond = self.expr()?;
                let header = start.to(self.prev_span());
                self.expect(TokenKind::Colon, "after the loop condition")?;
                let body = self.block()?;
                return Ok(Stmt { kind: StmtKind::While { cond, body }, span: header });
            }
            TokenKind::For => {
                self.bump();
                let target = self.for_target()?;
                self.expect(TokenKind::In, "after the loop variable")?;
                let iter = self.expr()?;
                let header = start.to(self.prev_span());
                self.expect(TokenKind::Colon, "after the loop's iterable")?;
                let body = self.block()?;
                return Ok(Stmt { kind: StmtKind::For { target, iter, body }, span: header });
            }
            TokenKind::Break => {
                self.bump();
                StmtKind::Break
            }
            TokenKind::Continue => {
                self.bump();
                StmtKind::Continue
            }
            TokenKind::Pass => {
                self.bump();
                StmtKind::Pass
            }
            TokenKind::Assert => return self.assert_stmt(),
            kind @ (TokenKind::Elif | TokenKind::Else) => {
                return Err(self.error(
                    format!("{} without a matching `if`", kind.describe()),
                    start,
                    "no `if` before this",
                    Some("`elif` and `else` must line up with the `if` they belong to"),
                ));
            }
            TokenKind::Match => return self.match_stmt(),
            TokenKind::Case => {
                return Err(self.error("`case` outside a `match`", start, "no `match` before this", None));
            }
            TokenKind::With => return self.with_stmt(),
            TokenKind::Def => {
                let function = self.function(Visibility::Default, false)?;
                let span = start.to(function.name.span);
                return Ok(Stmt { kind: StmtKind::Def(Box::new(function)), span });
            }
            kind @ (TokenKind::Struct
            | TokenKind::Trait
            | TokenKind::Impl
            | TokenKind::Enum
            | TokenKind::Const
            | TokenKind::Import
            | TokenKind::From
            | TokenKind::Pub
            | TokenKind::Priv
            | TokenKind::At) => {
                return Err(self.error(
                    format!("{} is not allowed inside a function", kind.describe()),
                    start,
                    "only allowed at the top level of a file",
                    None,
                ));
            }
            TokenKind::Name(name) if name == "try" && *self.peek_nth(1) == TokenKind::Colon => {
                return Err(self.error(
                    "there is no `try` statement",
                    start,
                    "not a keyword",
                    Some("functions that can fail return a `Result`, and `?` passes an error on to the caller"),
                ));
            }
            TokenKind::Name(_) if *self.peek_nth(1) == TokenKind::Colon => return self.annotated_assignment(),
            _ => return self.simple_statement(),
        };
        let span = start;
        self.end_of_statement()?;
        Ok(Stmt { kind, span })
    }

    /// `with value as name:` or `with value:`.
    fn with_stmt(&mut self) -> PResult<Stmt> {
        let start = self.bump().span;
        let value = self.expr()?;
        let target = if self.eat(&TokenKind::As) { Some(self.expect_name("a name after `as`")?) } else { None };
        self.expect(TokenKind::Colon, "after the `with` value")?;
        let body = self.block()?;
        Ok(Stmt { kind: StmtKind::With { value, target, body }, span: start })
    }

    fn if_stmt(&mut self) -> PResult<Stmt> {
        let start = self.bump().span;
        let cond = self.expr()?;
        let header = start.to(self.prev_span());
        self.expect(TokenKind::Colon, "after the condition")?;
        let mut branches = vec![(cond, self.block()?)];
        while self.at(&TokenKind::Elif) {
            self.bump();
            let cond = self.expr()?;
            self.expect(TokenKind::Colon, "after the condition")?;
            branches.push((cond, self.block()?));
        }
        let otherwise = if self.eat(&TokenKind::Else) {
            self.expect(TokenKind::Colon, "after `else`")?;
            Some(self.block()?)
        } else {
            None
        };
        Ok(Stmt { kind: StmtKind::If { branches, otherwise }, span: header })
    }

    fn match_stmt(&mut self) -> PResult<Stmt> {
        let start = self.bump().span;
        let subject = self.expr_list()?;
        let header = start.to(self.prev_span());
        self.expect(TokenKind::Colon, "after the value being matched")?;
        self.block_start()?;
        let mut cases = Vec::new();
        loop {
            match self.peek() {
                TokenKind::Dedent => {
                    self.bump();
                    break;
                }
                TokenKind::Eof => break,
                TokenKind::Newline => {
                    self.bump();
                }
                TokenKind::Case => match self.case() {
                    Ok(case) => cases.push(case),
                    Err(Reported) => self.skip_statement(),
                },
                _ => {
                    self.unexpected("expected `case`");
                    self.skip_statement();
                }
            }
        }
        if cases.is_empty() {
            return Err(self.error("a `match` needs at least one `case`", header, "no cases", None));
        }
        Ok(Stmt { kind: StmtKind::Match { subject, cases }, span: header })
    }

    fn case(&mut self) -> PResult<Case> {
        self.bump();
        let pattern = self.pattern()?;
        let guard = if self.eat(&TokenKind::If) { Some(self.expr()?) } else { None };
        self.expect(TokenKind::Colon, "after the pattern")?;
        let body = self.block()?;
        Ok(Case { pattern, guard, body })
    }

    fn pattern(&mut self) -> PResult<Pattern> {
        let first = self.closed_pattern()?;
        if !self.at(&TokenKind::Pipe) {
            return Ok(first);
        }
        let mut alternatives = vec![first];
        while self.eat(&TokenKind::Pipe) {
            alternatives.push(self.closed_pattern()?);
        }
        let span = alternatives[0].span.to(alternatives.last().unwrap().span);
        Ok(Pattern { kind: PatternKind::Or(alternatives), span })
    }

    fn closed_pattern(&mut self) -> PResult<Pattern> {
        let start = self.span();
        let literal = |kind: ExprKind, span: Span| Pattern { kind: PatternKind::Literal(Expr { kind, span }), span };
        let pattern = match self.peek().clone() {
            TokenKind::Name(name) if name == "_" => {
                self.bump();
                Pattern { kind: PatternKind::Wild, span: start }
            }
            TokenKind::Name(_) | TokenKind::NoneKw => {
                let first = if self.at(&TokenKind::NoneKw) {
                    self.bump();
                    Ident { name: "None".to_string(), span: start }
                } else {
                    self.expect_name("a name")?
                };
                let mut path = vec![first];
                while self.eat(&TokenKind::Dot) {
                    path.push(self.expect_name("a variant name after `.`")?);
                }
                let args = if self.eat(&TokenKind::LParen) {
                    let mut args = Vec::new();
                    while !self.at(&TokenKind::RParen) {
                        if matches!(self.peek(), TokenKind::Name(_)) && *self.peek_nth(1) == TokenKind::Assign {
                            let span = self.span();
                            return Err(self.not_supported("keyword patterns (`field=pattern`) are", span));
                        }
                        args.push(self.pattern()?);
                        if !self.eat(&TokenKind::Comma) {
                            break;
                        }
                    }
                    self.expect(TokenKind::RParen, "to close the pattern")?;
                    Some(args)
                } else {
                    None
                };
                let span = start.to(self.prev_span());
                if path.len() == 1 && args.is_none() && path[0].name != "None" {
                    Pattern { kind: PatternKind::Name(path.pop().unwrap()), span }
                } else {
                    Pattern { kind: PatternKind::Variant { path, args }, span }
                }
            }
            TokenKind::Int(value) => {
                self.bump();
                literal(ExprKind::Int(value), start)
            }
            TokenKind::Float(value) => {
                self.bump();
                literal(ExprKind::Float(value), start)
            }
            TokenKind::Str(value) => {
                self.bump();
                literal(ExprKind::Str(value), start)
            }
            TokenKind::True | TokenKind::False => {
                let value = self.at(&TokenKind::True);
                self.bump();
                literal(ExprKind::Bool(value), start)
            }
            TokenKind::Minus if matches!(self.peek_nth(1), TokenKind::Int(_) | TokenKind::Float(_)) => {
                self.bump();
                let operand = self.primary()?;
                let span = start.to(operand.span);
                literal(ExprKind::Unary { op: UnaryOp::Neg, operand: Box::new(operand) }, span)
            }
            TokenKind::LParen => {
                self.bump();
                let first = self.pattern()?;
                if !self.at(&TokenKind::Comma) {
                    self.expect(TokenKind::RParen, "to close `(`")?;
                    return Ok(first);
                }
                let mut items = vec![first];
                while self.eat(&TokenKind::Comma) {
                    if self.at(&TokenKind::RParen) {
                        break;
                    }
                    items.push(self.pattern()?);
                }
                let close = self.expect(TokenKind::RParen, "to close the tuple pattern")?;
                Pattern { kind: PatternKind::Tuple(items), span: start.to(close) }
            }
            TokenKind::LBracket | TokenKind::LBrace => {
                return Err(self.not_supported("list and dict patterns are", start));
            }
            _ => return Err(self.unexpected("expected a pattern")),
        };
        Ok(pattern)
    }

    fn annotated_assignment(&mut self) -> PResult<Stmt> {
        let name = self.expect_name("a variable name")?;
        self.bump(); // `:`
        let ty = self.type_expr()?;
        if !self.at(&TokenKind::Assign) {
            if self.at_line_end() {
                return Err(self.error(
                    format!("`{}` is declared without a value", name.name),
                    name.span,
                    "needs a value",
                    Some(&format!("give it a starting value: `{}: ... = value`", name.name)),
                ));
            }
            return Err(self.unexpected("expected `=`"));
        }
        self.bump();
        let value = self.expr_list()?;
        let span = name.span.to(value.span);
        self.end_of_statement()?;
        let target = Target { span: name.span, kind: TargetKind::Name(name) };
        Ok(Stmt { kind: StmtKind::Assign { target, ty: Some(ty), value }, span })
    }

    fn simple_statement(&mut self) -> PResult<Stmt> {
        let lhs = self.expr_list()?;
        let aug = match self.peek() {
            TokenKind::PlusEq => Some(BinaryOp::Add),
            TokenKind::MinusEq => Some(BinaryOp::Sub),
            TokenKind::StarEq => Some(BinaryOp::Mul),
            TokenKind::SlashEq => Some(BinaryOp::Div),
            TokenKind::SlashSlashEq => Some(BinaryOp::FloorDiv),
            TokenKind::PercentEq => Some(BinaryOp::Mod),
            _ => None,
        };
        if let Some(op) = aug {
            self.bump();
            let target = self.target_from(lhs)?;
            if matches!(target.kind, TargetKind::Tuple(_)) {
                return Err(self.error(
                    "augmented assignment needs a single target",
                    target.span,
                    "more than one target",
                    None,
                ));
            }
            let value = self.expr()?;
            let span = target.span.to(value.span);
            self.end_of_statement()?;
            return Ok(Stmt { kind: StmtKind::AugAssign { target, op, value }, span });
        }
        if self.at(&TokenKind::Assign) {
            self.bump();
            let target = self.target_from(lhs)?;
            let value = self.expr_list()?;
            if self.at(&TokenKind::Assign) {
                let span = self.span();
                return Err(self.not_supported("chained assignment (`a = b = c`) is", span));
            }
            let span = target.span.to(value.span);
            self.end_of_statement()?;
            return Ok(Stmt { kind: StmtKind::Assign { target, ty: None, value }, span });
        }
        let span = lhs.span;
        self.end_of_statement()?;
        Ok(Stmt { kind: StmtKind::Expr(lhs), span })
    }

    fn target_from(&mut self, expr: Expr) -> PResult<Target> {
        let span = expr.span;
        let kind = match expr.kind {
            ExprKind::Name(name) => TargetKind::Name(Ident { name, span }),
            ExprKind::Attr { object, name } => TargetKind::Attr { object: *object, field: name },
            ExprKind::Index { object, index } => TargetKind::Index { object: *object, index: *index },
            ExprKind::Tuple(items) => {
                let mut targets = Vec::new();
                for item in items {
                    targets.push(self.target_from(item)?);
                }
                TargetKind::Tuple(targets)
            }
            ExprKind::Slice { .. } => return Err(self.not_supported("assigning to a slice is", span)),
            _ => {
                return Err(self.error(
                    "cannot assign to this",
                    span,
                    "not a variable, field or element",
                    Some("the left side of `=` must be a name, `object.field` or `container[index]`"),
                ));
            }
        };
        Ok(Target { kind, span })
    }

    /// `for <targets> in`: names (or fields) separated by commas. Parsed without `in` as an
    /// operator, since `in` ends the targets.
    fn for_target(&mut self) -> PResult<Target> {
        let first = self.postfix()?;
        if !self.at(&TokenKind::Comma) {
            return self.target_from(first);
        }
        let mut items = vec![first];
        while self.eat(&TokenKind::Comma) {
            if self.at(&TokenKind::In) {
                break;
            }
            items.push(self.postfix()?);
        }
        let span = items[0].span.to(items.last().unwrap().span);
        self.target_from(Expr { kind: ExprKind::Tuple(items), span })
    }

    // ---- expressions --------------------------------------------------------------------

    /// One expression, or several separated by commas, which form a tuple: `return a, b`.
    fn expr_list(&mut self) -> PResult<Expr> {
        let first = self.expr()?;
        if !self.at(&TokenKind::Comma) {
            return Ok(first);
        }
        let mut items = vec![first];
        while self.eat(&TokenKind::Comma) {
            if self.at_line_end() || matches!(self.peek(), TokenKind::Assign) {
                break;
            }
            items.push(self.expr()?);
        }
        let span = items[0].span.to(items.last().unwrap().span);
        Ok(Expr { kind: ExprKind::Tuple(items), span })
    }

    fn expr(&mut self) -> PResult<Expr> {
        if self.at(&TokenKind::Lambda) {
            let start = self.bump().span;
            let mut params = Vec::new();
            while !self.at(&TokenKind::Colon) {
                params.push(self.expect_name("a parameter name")?);
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::Colon, "after the lambda's parameters")?;
            let body = self.expr()?;
            let span = start.to(body.span);
            return Ok(Expr { kind: ExprKind::Lambda { params, body: Box::new(body) }, span });
        }
        let value = self.binary(PREC_OR)?;
        if !self.at(&TokenKind::If) {
            return Ok(value);
        }
        self.bump();
        let cond = self.binary(PREC_OR)?;
        self.expect(TokenKind::Else, "in a conditional expression (`a if condition else b`)")?;
        let otherwise = self.expr()?;
        let span = value.span.to(otherwise.span);
        Ok(Expr {
            kind: ExprKind::IfElse { cond: Box::new(cond), then: Box::new(value), otherwise: Box::new(otherwise) },
            span,
        })
    }

    fn binary_op(&self) -> Option<(BinaryOp, u8, usize)> {
        use BinaryOp::*;
        Some(match self.peek() {
            TokenKind::Or => (Or, PREC_OR, 1),
            TokenKind::And => (And, PREC_AND, 1),
            TokenKind::EqEq => (Eq, PREC_COMPARE, 1),
            TokenKind::NotEq => (NotEq, PREC_COMPARE, 1),
            TokenKind::Lt => (Lt, PREC_COMPARE, 1),
            TokenKind::Le => (Le, PREC_COMPARE, 1),
            TokenKind::Gt => (Gt, PREC_COMPARE, 1),
            TokenKind::Ge => (Ge, PREC_COMPARE, 1),
            TokenKind::In => (In, PREC_COMPARE, 1),
            TokenKind::Not if *self.peek_nth(1) == TokenKind::In => (NotIn, PREC_COMPARE, 2),
            TokenKind::Is if *self.peek_nth(1) == TokenKind::Not => (IsNot, PREC_COMPARE, 2),
            TokenKind::Is => (Is, PREC_COMPARE, 1),
            TokenKind::Pipe => (BitOr, 5, 1),
            TokenKind::Caret => (BitXor, 6, 1),
            TokenKind::Amp => (BitAnd, 7, 1),
            TokenKind::Shl => (Shl, 8, 1),
            TokenKind::Shr => (Shr, 8, 1),
            TokenKind::Plus => (Add, 9, 1),
            TokenKind::Minus => (Sub, 9, 1),
            TokenKind::Star => (Mul, 10, 1),
            TokenKind::Slash => (Div, 10, 1),
            TokenKind::SlashSlash => (FloorDiv, 10, 1),
            TokenKind::Percent => (Mod, 10, 1),
            TokenKind::StarStar => (Pow, PREC_POW, 1),
            _ => return None,
        })
    }

    fn binary(&mut self, min_prec: u8) -> PResult<Expr> {
        let start = self.span();
        let unary = match self.peek() {
            TokenKind::Not => Some((UnaryOp::Not, PREC_NOT)),
            TokenKind::Minus => Some((UnaryOp::Neg, PREC_UNARY)),
            TokenKind::Plus => Some((UnaryOp::Plus, PREC_UNARY)),
            TokenKind::Tilde => Some((UnaryOp::Invert, PREC_UNARY)),
            _ => None,
        };
        let mut lhs = if let Some((op, prec)) = unary {
            self.bump();
            let operand = self.binary(prec)?;
            Expr { span: start.to(operand.span), kind: ExprKind::Unary { op, operand: Box::new(operand) } }
        } else if self.at(&TokenKind::Spawn) {
            self.bump();
            let call = self.postfix()?;
            if !matches!(call.kind, ExprKind::Call { .. }) {
                return Err(self.error(
                    "`spawn` needs a function call",
                    call.span,
                    "not a call",
                    Some("write `spawn worker(args)`; to run a block of code, put it in a function"),
                ));
            }
            Expr { span: start.to(call.span), kind: ExprKind::Spawn(Box::new(call)) }
        } else {
            self.postfix()?
        };

        let mut lhs_is_comparison = false;
        while let Some((op, prec, token_count)) = self.binary_op() {
            if prec < min_prec {
                break;
            }
            let op_span = self.span();
            for _ in 0..token_count {
                self.bump();
            }
            if op.is_comparison() && lhs_is_comparison {
                self.diags.push(
                    Diagnostic::error(
                        codes::COMPARISON_CHAIN,
                        "comparison chains are not supported",
                        lhs.span.to(op_span),
                        "second comparison",
                    )
                    .with_help("write `a < b and b < c` instead of `a < b < c`"),
                );
                return Err(Reported);
            }
            let next_min = if op == BinaryOp::Pow { prec } else { prec + 1 };
            let rhs = self.binary(next_min)?;
            lhs = Expr {
                span: lhs.span.to(rhs.span),
                kind: ExprKind::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs) },
            };
            lhs_is_comparison = op.is_comparison();
        }
        Ok(lhs)
    }

    fn postfix(&mut self) -> PResult<Expr> {
        let mut expr = self.primary()?;
        loop {
            let span = self.span();
            match self.peek() {
                TokenKind::LParen => {
                    self.bump();
                    let mut args = Vec::new();
                    while !self.at(&TokenKind::RParen) {
                        let name =
                            if matches!(self.peek(), TokenKind::Name(_)) && *self.peek_nth(1) == TokenKind::Assign {
                                let name = self.expect_name("an argument name")?;
                                self.bump();
                                Some(name)
                            } else {
                                None
                            };
                        let value = self.expr()?;
                        args.push(Arg { name, value });
                        if !self.eat(&TokenKind::Comma) {
                            break;
                        }
                    }
                    let close = self.expect(TokenKind::RParen, "to close the argument list")?;
                    expr = Expr { span: expr.span.to(close), kind: ExprKind::Call { callee: Box::new(expr), args } };
                }
                TokenKind::Dot => {
                    self.bump();
                    let name = self.expect_name("a field or method name after `.`")?;
                    expr =
                        Expr { span: expr.span.to(name.span), kind: ExprKind::Attr { object: Box::new(expr), name } };
                }
                TokenKind::LBracket => {
                    self.bump();
                    let start = if self.at(&TokenKind::Colon) { None } else { Some(Box::new(self.expr()?)) };
                    if self.eat(&TokenKind::Colon) {
                        let stop = if matches!(self.peek(), TokenKind::RBracket | TokenKind::Colon) {
                            None
                        } else {
                            Some(Box::new(self.expr()?))
                        };
                        if self.at(&TokenKind::Colon) {
                            let span = self.span();
                            return Err(self.not_supported("slice steps (`[a:b:step]`) are", span));
                        }
                        let close = self.expect(TokenKind::RBracket, "to close the slice")?;
                        expr = Expr {
                            span: expr.span.to(close),
                            kind: ExprKind::Slice { object: Box::new(expr), start, stop },
                        };
                    } else {
                        // `x[a, b]` indexes with a tuple; it is how `dict[str, int]` is written
                        // as an expression, for type arguments.
                        let mut start = start;
                        if self.at(&TokenKind::Comma) {
                            let first = *start.take().expect("an index expression");
                            let first_span = first.span;
                            let mut items = vec![first];
                            while self.eat(&TokenKind::Comma) {
                                if self.at(&TokenKind::RBracket) {
                                    break;
                                }
                                items.push(self.expr()?);
                            }
                            let span = first_span.to(self.prev_span());
                            start = Some(Box::new(Expr { kind: ExprKind::Tuple(items), span }));
                        }
                        let close = self.expect(TokenKind::RBracket, "to close the index")?;
                        let index = start.expect("an index without `:` has an expression");
                        expr =
                            Expr { span: expr.span.to(close), kind: ExprKind::Index { object: Box::new(expr), index } };
                    }
                }
                TokenKind::Question => {
                    self.bump();
                    expr = Expr { span: expr.span.to(span), kind: ExprKind::Try(Box::new(expr)) };
                }
                _ => return Ok(expr),
            }
        }
    }

    /// Comma-separated expressions up to `close`, allowing a trailing comma.
    fn sequence(&mut self, close: TokenKind, first: Expr, what: &str) -> PResult<(Vec<Expr>, Span)> {
        let mut items = vec![first];
        while self.eat(&TokenKind::Comma) {
            if self.at(&close) {
                break;
            }
            items.push(self.expr()?);
        }
        let end = self.expect(close, &format!("to close the {what}"))?;
        Ok((items, end))
    }

    fn primary(&mut self) -> PResult<Expr> {
        let span = self.span();
        let kind = match self.peek().clone() {
            TokenKind::Int(value) => ExprKind::Int(value),
            TokenKind::Float(value) => ExprKind::Float(value),
            TokenKind::Str(value) => ExprKind::Str(value),
            TokenKind::FString(pieces) => {
                self.bump();
                return Ok(Expr { kind: ExprKind::FString(self.fstring(pieces, span)?), span });
            }
            TokenKind::True => ExprKind::Bool(true),
            TokenKind::False => ExprKind::Bool(false),
            TokenKind::NoneKw => ExprKind::None,
            TokenKind::Name(name) => ExprKind::Name(name),
            TokenKind::SelfKw => ExprKind::Name("self".to_string()),
            TokenKind::LParen => {
                self.bump();
                if self.at(&TokenKind::RParen) {
                    return Err(self.not_supported("empty tuples are", span.to(self.span())));
                }
                let first = self.expr()?;
                if !self.at(&TokenKind::Comma) {
                    let close = self.expect(TokenKind::RParen, "to close `(`")?;
                    return Ok(Expr { span: span.to(close), kind: first.kind });
                }
                let (items, close) = self.sequence(TokenKind::RParen, first, "tuple")?;
                return Ok(Expr { kind: ExprKind::Tuple(items), span: span.to(close) });
            }
            TokenKind::LBracket => {
                self.bump();
                if self.at(&TokenKind::RBracket) {
                    let close = self.bump().span;
                    return Ok(Expr { kind: ExprKind::List(Vec::new()), span: span.to(close) });
                }
                let first = self.expr()?;
                if self.at(&TokenKind::For) {
                    let clauses = self.comp_clauses()?;
                    let close = self.expect(TokenKind::RBracket, "to close the list comprehension")?;
                    return Ok(Expr {
                        kind: ExprKind::ListComp { element: Box::new(first), clauses },
                        span: span.to(close),
                    });
                }
                let (items, close) = self.sequence(TokenKind::RBracket, first, "list")?;
                return Ok(Expr { kind: ExprKind::List(items), span: span.to(close) });
            }
            TokenKind::LBrace => {
                self.bump();
                if self.at(&TokenKind::RBrace) {
                    let close = self.bump().span;
                    return Ok(Expr { kind: ExprKind::Dict(Vec::new()), span: span.to(close) });
                }
                let key = self.expr()?;
                if !self.at(&TokenKind::Colon) {
                    return Err(self.not_supported("set literals are", span));
                }
                self.bump();
                let value = self.expr()?;
                if self.at(&TokenKind::For) {
                    let clauses = self.comp_clauses()?;
                    let close = self.expect(TokenKind::RBrace, "to close the dict comprehension")?;
                    return Ok(Expr {
                        kind: ExprKind::DictComp { key: Box::new(key), value: Box::new(value), clauses },
                        span: span.to(close),
                    });
                }
                let mut pairs = vec![(key, value)];
                while self.eat(&TokenKind::Comma) {
                    if self.at(&TokenKind::RBrace) {
                        break;
                    }
                    let key = self.expr()?;
                    self.expect(TokenKind::Colon, "between a key and its value")?;
                    let value = self.expr()?;
                    pairs.push((key, value));
                }
                let close = self.expect(TokenKind::RBrace, "to close the dict")?;
                return Ok(Expr { kind: ExprKind::Dict(pairs), span: span.to(close) });
            }
            TokenKind::Spawn => return Err(self.not_supported("`spawn` is", span)),
            _ => return Err(self.unexpected("expected an expression")),
        };
        self.bump();
        Ok(Expr { kind, span })
    }

    /// `for target in iterable if condition ...`, one or more times.
    fn comp_clauses(&mut self) -> PResult<Vec<CompClause>> {
        let mut clauses = Vec::new();
        while self.eat(&TokenKind::For) {
            let target = self.for_target()?;
            self.expect(TokenKind::In, "after the comprehension's variable")?;
            // `or`-level expressions, so that a following `if` starts a condition rather than
            // a conditional expression.
            let iter = self.binary(PREC_OR)?;
            let mut conditions = Vec::new();
            while self.eat(&TokenKind::If) {
                conditions.push(self.binary(PREC_OR)?);
            }
            clauses.push(CompClause { target, iter, conditions });
        }
        Ok(clauses)
    }

    /// Parse the `{expression}` pieces of an f-string with a sub-parser over their own tokens.
    fn fstring(&mut self, pieces: Vec<FPiece>, span: Span) -> PResult<Vec<FStringPart>> {
        let mut parts = Vec::new();
        let mut failed = false;
        for piece in pieces {
            match piece {
                FPiece::Literal(text) => parts.push(FStringPart::Literal(text)),
                FPiece::Code { text, offset, spec } => {
                    let tokens = lex_fragment(span.file, &text, offset, self.diags);
                    let mut sub = Parser { tokens: &tokens, pos: 0, line_start: offset as u32, diags: self.diags };
                    let expr = match sub.expr() {
                        Ok(expr) if sub.at(&TokenKind::Eof) => expr,
                        Ok(_) => {
                            sub.unexpected("expected `}` after the expression");
                            failed = true;
                            continue;
                        }
                        Err(Reported) => {
                            failed = true;
                            continue;
                        }
                    };
                    let spec = spec.map(|(text, at)| {
                        let len = text.len();
                        (text, Span::new(span.file, at, at + len))
                    });
                    parts.push(FStringPart::Expr { expr, spec });
                }
            }
        }
        if failed { Err(Reported) } else { Ok(parts) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;
    use lang_diag::FileId;

    fn parse_text(text: &str) -> (Module, Diagnostics) {
        let mut diags = Diagnostics::default();
        let tokens = lex(FileId(0), text, &mut diags);
        let module = parse(&tokens, &mut diags);
        (module, diags)
    }

    fn codes_of(diags: &Diagnostics) -> Vec<&'static str> {
        diags.sorted().iter().map(|d| d.code).collect()
    }

    /// Render an expression with full parenthesisation, to check precedence.
    fn show(expr: &Expr) -> String {
        match &expr.kind {
            ExprKind::Int(v) => v.to_string(),
            ExprKind::Float(v) => format!("{v:?}"),
            ExprKind::Str(s) => format!("{s:?}"),
            ExprKind::FString(parts) => format!("f[{}]", parts.len()),
            ExprKind::Bool(b) => if *b { "True" } else { "False" }.to_string(),
            ExprKind::None => "None".to_string(),
            ExprKind::Name(n) => n.clone(),
            ExprKind::Call { callee, args } => {
                let args: Vec<String> = args
                    .iter()
                    .map(|a| match &a.name {
                        Some(name) => format!("{}={}", name.name, show(&a.value)),
                        None => show(&a.value),
                    })
                    .collect();
                format!("{}({})", show(callee), args.join(", "))
            }
            ExprKind::Attr { object, name } => format!("{}.{}", show(object), name.name),
            ExprKind::Index { object, index } => format!("{}[{}]", show(object), show(index)),
            ExprKind::Slice { object, start, stop } => format!(
                "{}[{}:{}]",
                show(object),
                start.as_ref().map_or(String::new(), |e| show(e)),
                stop.as_ref().map_or(String::new(), |e| show(e))
            ),
            ExprKind::List(items) => format!("[{}]", items.iter().map(show).collect::<Vec<_>>().join(", ")),
            ExprKind::Tuple(items) => format!("tuple({})", items.iter().map(show).collect::<Vec<_>>().join(", ")),
            ExprKind::Dict(pairs) => {
                format!(
                    "{{{}}}",
                    pairs.iter().map(|(k, v)| format!("{}: {}", show(k), show(v))).collect::<Vec<_>>().join(", ")
                )
            }
            ExprKind::Unary { op, operand } => format!("({} {})", op.symbol(), show(operand)),
            ExprKind::Binary { op, lhs, rhs } => format!("({} {} {})", show(lhs), op.symbol(), show(rhs)),
            ExprKind::IfElse { cond, then, otherwise } => {
                format!("({} if {} else {})", show(then), show(cond), show(otherwise))
            }
            ExprKind::Try(inner) => format!("{}?", show(inner)),
            ExprKind::Spawn(inner) => format!("spawn {}", show(inner)),
            ExprKind::Lambda { params, body } => {
                format!(
                    "(lambda {}: {})",
                    params.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", "),
                    show(body)
                )
            }
            ExprKind::ListComp { element, clauses } => format!("[{} for {}]", show(element), clauses.len()),
            ExprKind::DictComp { key, value, clauses } => {
                format!("{{{}: {} for {}}}", show(key), show(value), clauses.len())
            }
        }
    }

    fn show_pattern(pattern: &Pattern) -> String {
        match &pattern.kind {
            PatternKind::Wild => "_".into(),
            PatternKind::Name(name) => name.name.clone(),
            PatternKind::Literal(e) => show(e),
            PatternKind::Variant { path, args } => {
                let path: Vec<&str> = path.iter().map(|p| p.name.as_str()).collect();
                match args {
                    Some(args) => {
                        format!("{}({})", path.join("."), args.iter().map(show_pattern).collect::<Vec<_>>().join(", "))
                    }
                    None => path.join("."),
                }
            }
            PatternKind::Tuple(items) => format!("({})", items.iter().map(show_pattern).collect::<Vec<_>>().join(", ")),
            PatternKind::Or(alts) => alts.iter().map(show_pattern).collect::<Vec<_>>().join(" | "),
        }
    }

    #[test]
    fn enums_traits_and_impls() {
        let text = "\
@derive(Eq, Display)
enum Shape:
    Circle(radius: float)
    Square(side: float)
    Empty

trait Named(Display):
    def name(self) -> str

    def greet(self) -> str:
        return \"hi \" + self.name()

impl Named for Shape:
    def name(self) -> str:
        return \"shape\"
";
        let (module, diags) = parse_text(text);
        assert!(diags.is_empty(), "{:?}", codes_of(&diags));
        let Item::Enum(shape) = &module.items[0] else { panic!() };
        assert_eq!(shape.annotations[0].name.name, "derive");
        assert_eq!(shape.annotations[0].args.len(), 2);
        assert_eq!(shape.variants.len(), 3);
        assert_eq!(shape.variants[0].fields.len(), 1);
        let Item::Trait(named) = &module.items[1] else { panic!() };
        assert_eq!(named.supertraits[0].name, "Display");
        assert!(!named.methods[0].has_body && named.methods[1].has_body);
        let Item::Impl(imp) = &module.items[2] else { panic!() };
        assert_eq!(imp.trait_name.as_ref().unwrap().name, "Named");
        assert_eq!(imp.target.name, "Shape");
    }

    #[test]
    fn match_and_patterns() {
        let text = "\
def main():
    match value:
        case Circle(r) if r > 1.0:
            x = 1
        case Shape.Square(_) | Empty:
            x = 2
        case (1, \"a\", -2, True):
            x = 3
        case Some(Ok(x)) | None:
            x = 4
        case other:
            x = 5
";
        let (module, diags) = parse_text(text);
        assert!(diags.is_empty(), "{:?}", codes_of(&diags));
        let StmtKind::Match { cases, .. } = &function(&module, 0).body[0].kind else { panic!() };
        let shown: Vec<String> = cases.iter().map(|c| show_pattern(&c.pattern)).collect();
        assert_eq!(
            shown,
            ["Circle(r)", "Shape.Square(_) | Empty", "(1, \"a\", (- 2), True)", "Some(Ok(x)) | None", "other"]
        );
        assert!(cases[0].guard.is_some());
    }

    #[test]
    fn generics_closures_and_comprehensions() {
        let text = "\
def first[T: Ord + Display, U](items: list[T], fallback: U = 0) -> T?:
    def inner(x: int) -> int:
        return x
    f: def(int, str) -> bool = lambda a, b: a > 0
    squares = [x * x for x in range(10) if x % 2 == 0]
    index = {name: i for i, name in pairs}
    return None

struct Stack[T]:
    items: list[T] = []

impl[T: Display] Stack[T]:
    def push(self, item: T):
        self.items.append(item)

impl[T] Display for Stack[T]:
    def to_str(self) -> str:
        return \"stack\"
";
        let (module, diags) = parse_text(text);
        assert!(diags.is_empty(), "{:?}", codes_of(&diags));
        let f = function(&module, 0);
        assert_eq!(f.generics.len(), 2);
        assert_eq!(f.generics[0].bounds.len(), 2);
        assert!(f.params[1].default.is_some());
        assert!(matches!(f.body[0].kind, StmtKind::Def(_)));
        assert!(matches!(
            &f.body[1].kind,
            StmtKind::Assign { ty: Some(TypeExpr { kind: TypeExprKind::Func { .. }, .. }), .. }
        ));
        assert!(matches!(
            &f.body[2].kind,
            StmtKind::Assign { value: Expr { kind: ExprKind::ListComp { .. }, .. }, .. }
        ));
        assert!(matches!(
            &f.body[3].kind,
            StmtKind::Assign { value: Expr { kind: ExprKind::DictComp { .. }, .. }, .. }
        ));
        let Item::Struct(stack) = &module.items[1] else { panic!() };
        assert_eq!(stack.generics.len(), 1);
        let Item::Impl(imp) = &module.items[2] else { panic!() };
        assert_eq!((imp.generics.len(), imp.target_args.len()), (1, 1));
        let Item::Impl(imp) = &module.items[3] else { panic!() };
        assert_eq!(imp.trait_name.as_ref().unwrap().name, "Display");
        assert_eq!(imp.target_args.len(), 1);
        assert_eq!(expr_of("[a if c else b for a, b in xs if a]"), "[(a if c else b) for 1]");
        assert_eq!(expr_of("sort(key=lambda u: u.age)"), "sort(key=(lambda u: u.age))");
    }

    #[test]
    fn question_mark_is_postfix() {
        assert_eq!(expr_of("load(id)?.name"), "load(id)?.name");
        assert_eq!(expr_of("int.parse(s)? + 1"), "(int.parse(s)? + 1)");
    }

    fn function(module: &Module, index: usize) -> &Function {
        match &module.items[index] {
            Item::Function(f) => f,
            other => panic!("expected a function, got {other:?}"),
        }
    }

    fn expr_of(text: &str) -> String {
        let (module, diags) = parse_text(&format!("def main():\n    {text}\n"));
        assert!(diags.is_empty(), "unexpected diagnostics: {:?}", codes_of(&diags));
        match &function(&module, 0).body[0].kind {
            StmtKind::Expr(e) => show(e),
            other => panic!("expected an expression statement, got {other:?}"),
        }
    }

    #[test]
    fn hello_world() {
        let (module, diags) = parse_text("def main():\n    print(\"Hello, world!\")\n");
        assert!(diags.is_empty());
        let f = function(&module, 0);
        assert_eq!(f.name.name, "main");
        assert!(f.params.is_empty() && f.ret.is_none());
        assert_eq!(f.body.len(), 1);
    }

    #[test]
    fn precedence() {
        assert_eq!(expr_of("1 + 2 * 3"), "(1 + (2 * 3))");
        assert_eq!(expr_of("1 - 2 - 3"), "((1 - 2) - 3)");
        assert_eq!(expr_of("2 ** 3 ** 2"), "(2 ** (3 ** 2))");
        assert_eq!(expr_of("-x ** 2"), "(- (x ** 2))");
        assert_eq!(expr_of("not a == b and c"), "((not (a == b)) and c)");
        assert_eq!(expr_of("a or b and c"), "(a or (b and c))");
        assert_eq!(expr_of("a not in b"), "(a not in b)");
        assert_eq!(expr_of("x is not None"), "(x is not None)");
        assert_eq!(expr_of("(1 + 2) * 3"), "((1 + 2) * 3)");
        assert_eq!(expr_of("f(1, g(2),)"), "f(1, g(2))");
        assert_eq!(expr_of("a if c else b if d else e"), "(a if c else (b if d else e))");
        assert_eq!(expr_of("user.name.upper()"), "user.name.upper()");
        assert_eq!(expr_of("xs[1:][0]"), "xs[1:][0]");
        assert_eq!(expr_of("s[:-1]"), "s[:(- 1)]");
        assert_eq!(expr_of("User(id=1, name=\"a\")"), "User(id=1, name=\"a\")");
        assert_eq!(expr_of("[1, 2,]"), "[1, 2]");
        assert_eq!(expr_of("{\"a\": 1, \"b\": 2}"), "{\"a\": 1, \"b\": 2}");
        assert_eq!(expr_of("(1,)"), "tuple(1)");
        assert_eq!(expr_of("f\"{a + 1:>4}!\""), "f[2]");
    }

    #[test]
    fn comparison_chain_is_rejected() {
        let (_, diags) = parse_text("def main():\n    a < b < c\n");
        assert_eq!(codes_of(&diags), [codes::COMPARISON_CHAIN]);
        let (_, diags) = parse_text("def main():\n    (a < b) < c\n");
        assert!(diags.is_empty());
    }

    #[test]
    fn statements() {
        let text = "\
def main():
    x: int = 1
    a, b = b, a
    p.x += 1
    xs[0] = 2
    if x > 1:
        return
    elif x < 0:
        x = 0
    else:
        x = 1
    while True:
        break
    for k, v in d.items():
        continue
";
        let (module, diags) = parse_text(text);
        assert!(diags.is_empty(), "{:?}", codes_of(&diags));
        let body = &function(&module, 0).body;
        assert!(matches!(body[0].kind, StmtKind::Assign { ty: Some(_), .. }));
        assert!(
            matches!(&body[1].kind, StmtKind::Assign { target: Target { kind: TargetKind::Tuple(t), .. }, .. } if t.len() == 2)
        );
        assert!(matches!(body[2].kind, StmtKind::AugAssign { op: BinaryOp::Add, .. }));
        assert!(matches!(body[3].kind, StmtKind::Assign { target: Target { kind: TargetKind::Index { .. }, .. }, .. }));
        assert!(matches!(&body[4].kind, StmtKind::If { branches, otherwise: Some(_) } if branches.len() == 2));
        assert!(matches!(body[5].kind, StmtKind::While { .. }));
        assert!(matches!(&body[6].kind, StmtKind::For { target: Target { kind: TargetKind::Tuple(_), .. }, .. }));
    }

    #[test]
    fn structs_impls_and_consts() {
        let text = "\
const LIMIT = 10

pub struct User:
    \"\"\"A user.\"\"\"
    id: int
    priv name: str
    tags: list[str] = []

impl User:
    def new(name: str) -> User:
        return User(id=1, name=name)

    pub def rename(self, name: str):
        self.name = name
";
        let (module, diags) = parse_text(text);
        assert!(diags.is_empty(), "{:?}", codes_of(&diags));
        assert!(matches!(module.items[0], Item::Const(_)));
        let Item::Struct(user) = &module.items[1] else { panic!() };
        assert_eq!(user.fields.len(), 3);
        assert_eq!(user.fields[1].visibility, Visibility::Priv);
        assert!(user.fields[2].default.is_some());
        let Item::Impl(imp) = &module.items[2] else { panic!() };
        assert_eq!(imp.methods.len(), 2);
        assert!(imp.methods[0].self_param.is_none());
        assert!(imp.methods[1].self_param.is_some());
    }

    #[test]
    fn errors_recover_at_the_next_statement() {
        let (module, diags) =
            parse_text("def main():\n    x = {1, 2}\n    print(1 +)\n    z()\n\ndef other():\n    w()\n");
        assert_eq!(codes_of(&diags), [codes::NOT_SUPPORTED_SYNTAX, codes::UNEXPECTED_TOKEN]);
        assert_eq!(module.items.len(), 2);
        assert_eq!(function(&module, 0).body.len(), 1, "only `z()` survives");
    }

    #[test]
    fn imports_annotations_and_with() {
        let (module, diags) = parse_text(
            "import models\nimport services.billing as billing\nfrom shapes import (Circle, Square as Sq,)\n\n@extern(\"labs\")\ndef c_labs(x: int) -> int\n\n@http.get(\"/users\", name=\"list\")\ndef users():\n    with open(\"x\") as f:\n        pass\n\ndef f(u: models.User):\n    pass\n",
        );
        assert_eq!(codes_of(&diags), Vec::<&str>::new());
        let Item::Import(first) = &module.items[0] else { panic!() };
        assert_eq!(first.path[0].name, "models");
        let Item::Import(second) = &module.items[1] else { panic!() };
        assert_eq!((second.path.len(), second.alias.as_ref().unwrap().name.as_str()), (2, "billing"));
        let Item::Import(third) = &module.items[2] else { panic!() };
        let names = third.names.as_ref().unwrap();
        assert_eq!(names.len(), 2);
        assert_eq!(names[1].1.as_ref().unwrap().name, "Sq");
        let Item::Function(extern_fn) = &module.items[3] else { panic!() };
        assert!(!extern_fn.has_body);
        assert_eq!(extern_fn.annotations[0].name.name, "extern");
        let Item::Function(handler) = &module.items[4] else { panic!() };
        assert_eq!(handler.annotations[0].name.name, "http.get");
        assert_eq!(handler.annotations[0].args.len(), 2);
        assert!(matches!(handler.body[0].kind, StmtKind::With { .. }));
        let Item::Function(f) = &module.items[5] else { panic!() };
        assert!(
            matches!(&f.params[0].ty.as_ref().unwrap().kind, TypeExprKind::Named { name, .. } if name == "models.User")
        );
    }

    #[test]
    fn f_string_expression_errors_point_into_the_string() {
        let (_, diags) = parse_text("def main():\n    print(f\"a{1 +}b\")\n");
        let sorted = diags.sorted();
        assert_eq!(sorted.len(), 1);
        assert_eq!(sorted[0].primary.span.lo, "def main():\n    print(f\"a{1 +".len() as u32);
    }

    #[test]
    fn top_level_statement_is_rejected() {
        let (_, diags) = parse_text("print(\"hi\")\n");
        assert_eq!(codes_of(&diags), [codes::UNEXPECTED_TOKEN]);
    }

    #[test]
    fn unexpected_indent_is_rejected() {
        let (module, diags) = parse_text("def main():\n    a()\n        b()\n    c()\n");
        assert_eq!(codes_of(&diags), [codes::UNEXPECTED_TOKEN]);
        assert_eq!(function(&module, 0).body.len(), 2);
    }
}
