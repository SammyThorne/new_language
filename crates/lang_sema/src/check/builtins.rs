//! Built-in functions (`print`, `len`, ...) and the methods of built-in types (`str`, `list`,
//! `dict`, optionals). Each is described by its parameter types and result type, then checked
//! like any other call.

use super::{Checker, FnCx, error_expr, plural};
use crate::hir::{self, Builtin as Op};
use crate::types::{Ty, TyKind};
use lang_diag::{Diagnostic, Span, codes};
use lang_syntax::ast;

const STR_METHODS: &[&str] = &[
    "upper",
    "lower",
    "strip",
    "lstrip",
    "rstrip",
    "split",
    "join",
    "replace",
    "find",
    "starts_with",
    "ends_with",
    "contains",
];
const LIST_METHODS: &[&str] =
    &["append", "extend", "insert", "pop", "remove", "clear", "sort", "reverse", "index", "copy"];
const DICT_METHODS: &[&str] = &["get", "get_or", "pop", "keys", "values", "items", "clear"];
const OPTION_METHODS: &[&str] = &["unwrap", "unwrap_or", "is_some", "is_none"];

impl Checker<'_, '_> {
    fn no_keywords(&mut self, args: &[ast::Arg], what: &str) -> bool {
        if let Some(arg) = args.iter().find(|a| a.name.is_some()) {
            let span = arg.name.as_ref().unwrap().span;
            self.error(Diagnostic::error(
                codes::NOT_SUPPORTED,
                format!("{what} takes no keyword arguments"),
                span,
                "not supported",
            ));
            return false;
        }
        true
    }

    fn arity(&mut self, name: &str, args: &[ast::Arg], expected: usize, span: Span) -> bool {
        if args.len() == expected {
            return true;
        }
        let given = args.len();
        let verb = if given == 1 { "was" } else { "were" };
        self.error(Diagnostic::error(
            codes::WRONG_ARG_COUNT,
            format!("`{name}` takes {} but {given} {verb} given", plural(expected, "argument")),
            span,
            format!("expected {}", plural(expected, "argument")),
        ));
        false
    }

    pub(super) fn builtin_function(&mut self, cx: &mut FnCx, name: &str, args: &[ast::Arg], span: Span) -> hir::Expr {
        let builtin =
            |op: Op, args: Vec<hir::Expr>, ty: Ty| hir::Expr { kind: hir::ExprKind::Builtin { op, args }, ty, span };
        if !self.no_keywords(args, &format!("`{name}`")) {
            for arg in args {
                self.expr(cx, &arg.value, None);
            }
            return error_expr(span);
        }
        if name == "print" {
            let mut values = Vec::new();
            for arg in args {
                let value = self.expr(cx, &arg.value, None);
                if value.ty != Ty::ERROR && !self.types.is_displayable(value.ty) {
                    self.not_displayable(value.ty, value.span);
                }
                values.push(value);
            }
            return builtin(Op::Print, values, Ty::NONE);
        }
        if name == "range" {
            self.error(
                Diagnostic::error(
                    codes::NOT_SUPPORTED,
                    "`range(...)` can only be used in a `for` loop for now",
                    span,
                    "outside a `for` loop",
                )
                .with_help("write `for i in range(n):`"),
            );
            return error_expr(span);
        }
        if !self.arity(name, args, 1, span) {
            for arg in args {
                self.expr(cx, &arg.value, None);
            }
            return error_expr(span);
        }
        let arg = &args[0].value;
        if name == "panic" {
            let message = self.expect(cx, arg, Ty::STR, None);
            return builtin(Op::Panic, vec![message], Ty::NONE);
        }
        let value = self.expr(cx, arg, if name == "float" { Some(Ty::FLOAT) } else { None });
        if value.ty == Ty::ERROR {
            return error_expr(span);
        }
        let kind = self.types.kind(value.ty).clone();
        let result = match (name, &kind) {
            ("len", TyKind::Str | TyKind::List(_) | TyKind::Dict(..)) => builtin(Op::Len, vec![value], Ty::INT),
            ("str", TyKind::Str) => value,
            ("str", _) if self.types.is_displayable(value.ty) => builtin(Op::ToStr, vec![value], Ty::STR),
            ("int", TyKind::Int) => value,
            ("int", TyKind::Float) => builtin(Op::IntOfFloat, vec![value], Ty::INT),
            ("int", TyKind::Bool) => builtin(Op::IntOfBool, vec![value], Ty::INT),
            ("float", TyKind::Float) => value,
            ("float", TyKind::Int) => builtin(Op::FloatOfInt, vec![value], Ty::FLOAT),
            ("abs", TyKind::Int) => builtin(Op::AbsInt, vec![value], Ty::INT),
            ("abs", TyKind::Float) => builtin(Op::AbsFloat, vec![value], Ty::FLOAT),
            ("str", _) => {
                self.not_displayable(value.ty, value.span);
                return error_expr(span);
            }
            ("int" | "float", TyKind::Str) => {
                self.error(
                    Diagnostic::error(
                        codes::NOT_SUPPORTED,
                        "converting text to a number is not supported yet",
                        value.span,
                        "a `str`",
                    )
                    .with_help("parsing arrives as `int.parse(text)` with `Result` in milestone 3"),
                );
                return error_expr(span);
            }
            _ => {
                let shown = self.display(value.ty);
                let accepted = match name {
                    "len" => "a `str`, list or dict",
                    "int" => "a `float` or `bool`",
                    "float" => "an `int`",
                    _ => "an `int` or `float`",
                };
                self.error(Diagnostic::error(
                    codes::TYPE_MISMATCH,
                    format!("`{name}` cannot take a `{shown}`"),
                    value.span,
                    format!("expected {accepted}"),
                ));
                return error_expr(span);
            }
        };
        hir::Expr { span, ..result }
    }

    pub(super) fn is_builtin_method(&self, ty: Ty, name: &str) -> bool {
        let methods = match self.types.kind(ty) {
            TyKind::Str => STR_METHODS,
            TyKind::List(_) => LIST_METHODS,
            TyKind::Dict(..) => DICT_METHODS,
            TyKind::Option(_) => OPTION_METHODS,
            _ => return false,
        };
        methods.contains(&name)
    }

    pub(super) fn builtin_method(
        &mut self,
        cx: &mut FnCx,
        receiver: hir::Expr,
        name: &ast::Ident,
        args: &[ast::Arg],
        span: Span,
    ) -> hir::Expr {
        let ty = receiver.ty;
        let kind = self.types.kind(ty).clone();
        let n = args.len();
        let list_str = self.types.list(Ty::STR);
        // (operation, parameter types, result type)
        let signature: Option<(Op, Vec<Ty>, Ty)> = match (&kind, name.name.as_str()) {
            (TyKind::Str, "upper") => Some((Op::StrUpper, vec![], Ty::STR)),
            (TyKind::Str, "lower") => Some((Op::StrLower, vec![], Ty::STR)),
            (TyKind::Str, "strip") => Some((Op::StrStrip, vec![], Ty::STR)),
            (TyKind::Str, "lstrip") => Some((Op::StrLstrip, vec![], Ty::STR)),
            (TyKind::Str, "rstrip") => Some((Op::StrRstrip, vec![], Ty::STR)),
            (TyKind::Str, "split") if n == 0 => Some((Op::StrSplitWhitespace, vec![], list_str)),
            (TyKind::Str, "split") => Some((Op::StrSplit, vec![Ty::STR], list_str)),
            (TyKind::Str, "join") => Some((Op::StrJoin, vec![list_str], Ty::STR)),
            (TyKind::Str, "replace") => Some((Op::StrReplace, vec![Ty::STR, Ty::STR], Ty::STR)),
            (TyKind::Str, "find") => Some((Op::StrFind, vec![Ty::STR], Ty::INT)),
            (TyKind::Str, "starts_with") => Some((Op::StrStartsWith, vec![Ty::STR], Ty::BOOL)),
            (TyKind::Str, "ends_with") => Some((Op::StrEndsWith, vec![Ty::STR], Ty::BOOL)),
            (TyKind::Str, "contains") => Some((Op::StrContains, vec![Ty::STR], Ty::BOOL)),
            (TyKind::List(elem), method) => {
                let elem = *elem;
                match method {
                    "append" => Some((Op::ListAppend, vec![elem], Ty::NONE)),
                    "extend" => Some((Op::ListExtend, vec![ty], Ty::NONE)),
                    "insert" => Some((Op::ListInsert, vec![Ty::INT, elem], Ty::NONE)),
                    "pop" if n == 0 => Some((Op::ListPopLast, vec![], elem)),
                    "pop" => Some((Op::ListPopAt, vec![Ty::INT], elem)),
                    "remove" | "index" if !self.types.is_equatable(elem) => {
                        let shown = self.display(elem);
                        self.error(
                            Diagnostic::error(
                                codes::BAD_OPERANDS,
                                format!(
                                    "`{}` needs to compare elements, and `{shown}` values cannot be compared yet",
                                    name.name
                                ),
                                name.span,
                                "elements cannot be compared",
                            )
                            .with_help("comparing structs needs the `Eq` trait, which arrives with traits"),
                        );
                        return error_expr(span);
                    }
                    "remove" => Some((Op::ListRemove, vec![elem], Ty::NONE)),
                    "index" => Some((Op::ListIndex, vec![elem], Ty::INT)),
                    "clear" => Some((Op::ListClear, vec![], Ty::NONE)),
                    "sort" if !self.types.is_orderable(elem) => {
                        let shown = self.display(elem);
                        self.error(
                            Diagnostic::error(
                                codes::BAD_OPERANDS,
                                format!("a list of `{shown}` cannot be sorted yet"),
                                name.span,
                                "elements have no order",
                            )
                            .with_help("`sort` works on lists of numbers, strings and booleans"),
                        );
                        return error_expr(span);
                    }
                    "sort" => Some((Op::ListSort, vec![], Ty::NONE)),
                    "reverse" => Some((Op::ListReverse, vec![], Ty::NONE)),
                    "copy" => Some((Op::ListCopy, vec![], ty)),
                    _ => None,
                }
            }
            (TyKind::Dict(key, value), method) => {
                let (key, value) = (*key, *value);
                match method {
                    "get" => Some((Op::DictGet, vec![key], self.types.option(value))),
                    "get_or" => Some((Op::DictGetOr, vec![key, value], value)),
                    "pop" => Some((Op::DictPop, vec![key], self.types.option(value))),
                    "keys" => Some((Op::DictKeys, vec![], self.types.list(key))),
                    "values" => Some((Op::DictValues, vec![], self.types.list(value))),
                    "items" => {
                        let pair = self.types.tuple(vec![key, value]);
                        Some((Op::DictItems, vec![], self.types.list(pair)))
                    }
                    "clear" => Some((Op::DictClear, vec![], Ty::NONE)),
                    _ => None,
                }
            }
            (TyKind::Option(inner), method) => {
                let inner = *inner;
                match method {
                    "unwrap" => Some((Op::OptUnwrap, vec![], inner)),
                    "unwrap_or" => Some((Op::OptUnwrapOr, vec![inner], inner)),
                    "is_some" => Some((Op::OptIsSome, vec![], Ty::BOOL)),
                    "is_none" => Some((Op::OptIsNone, vec![], Ty::BOOL)),
                    _ => None,
                }
            }
            _ => None,
        };

        let Some((op, params, ret)) = signature else {
            return self.unknown_method(cx, ty, &kind, name, args, span);
        };
        if !self.no_keywords(args, &format!("`{}`", name.name)) || !self.arity(&name.name, args, params.len(), span) {
            for arg in args {
                self.expr(cx, &arg.value, None);
            }
            return error_expr(span);
        }
        let mut all = vec![receiver];
        for (arg, param) in args.iter().zip(params) {
            all.push(self.expect(cx, &arg.value, param, None));
        }
        hir::Expr { kind: hir::ExprKind::Builtin { op, args: all }, ty: ret, span }
    }

    fn unknown_method(
        &mut self,
        cx: &mut FnCx,
        ty: Ty,
        kind: &TyKind,
        name: &ast::Ident,
        args: &[ast::Arg],
        span: Span,
    ) -> hir::Expr {
        for arg in args {
            self.expr(cx, &arg.value, None);
        }
        let shown = self.display(ty);
        let methods = match kind {
            TyKind::Str => STR_METHODS,
            TyKind::List(_) => LIST_METHODS,
            TyKind::Dict(..) => DICT_METHODS,
            _ => OPTION_METHODS,
        };
        let help = match name.name.as_str() {
            "startswith" => Some("the method is called `starts_with`".to_string()),
            "endswith" => Some("the method is called `ends_with`".to_string()),
            "unwrap" | "unwrap_or" | "is_some" | "is_none" => None,
            other => super::closest(other, methods.iter().copied()).map(|s| format!("did you mean `{s}`?")),
        };
        let help = help.or_else(|| {
            matches!(kind, TyKind::Option(_)).then(|| {
                format!(
                    "this value may be `None`; get the `{}` with `.unwrap()` first",
                    self.display(self.types.option_inner(ty).unwrap())
                )
            })
        });
        let mut diagnostic = Diagnostic::error(
            codes::NO_SUCH_MEMBER,
            format!("`{shown}` has no method `{}`", name.name),
            name.span,
            "no such method",
        );
        if let Some(help) = help {
            diagnostic = diagnostic.with_help(help);
        }
        self.error(diagnostic);
        error_expr(span)
    }
}
