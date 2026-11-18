//! Expressions.
//!
//! Checking is bidirectional: `expected` carries the type the context wants, when known. It is a
//! hint, used to give `[]`, `{}`, `None` and integer literals their types; [`Checker::expect`]
//! and [`Checker::coerce`] then enforce it, applying the only implicit conversions the language
//! has: `T` to `T?`, and `None` to any optional type.

use super::{BUILTIN_FUNCTIONS, Checker, FnCx, Global, Reason, error_expr};
use crate::hir::{self, BinaryOp as B, ExprKind as K};
use crate::types::{Ty, TyKind};
use lang_diag::{Diagnostic, Span, codes};
use lang_format::{Kind, Spec};
use lang_syntax::ast;

/// Built-in functions from the design that are not implemented yet.
const PLANNED_FUNCTIONS: &[&str] = &["min", "max", "sorted", "sum", "enumerate", "zip", "any", "all", "open", "input"];

fn mk(kind: hir::ExprKind, ty: Ty, span: Span) -> hir::Expr {
    hir::Expr { kind, ty, span }
}

impl Checker<'_, '_> {
    pub(super) fn expect(&mut self, cx: &mut FnCx, expr: &ast::Expr, expected: Ty, reason: Reason) -> hir::Expr {
        let checked = self.expr(cx, expr, Some(expected));
        self.coerce(checked, expected, reason)
    }

    /// Accept `value` where `expected` is required, wrapping it as an option if needed, or report
    /// a mismatch.
    pub(super) fn coerce(&mut self, value: hir::Expr, expected: Ty, reason: Reason) -> hir::Expr {
        if self.types.fits(value.ty, expected) {
            return value;
        }
        if let Some(inner) = self.types.option_inner(expected) {
            if matches!(value.kind, K::None) && value.ty == Ty::NONE {
                return hir::Expr { ty: expected, ..value };
            }
            if self.types.fits(value.ty, inner) {
                let span = value.span;
                return mk(K::Some(Box::new(value)), expected, span);
            }
        }
        self.mismatch(value.span, expected, value.ty, reason);
        value
    }

    fn mismatch(&mut self, span: Span, expected: Ty, found: Ty, reason: Reason) {
        let (expected_shown, found_shown) = (self.display(expected), self.display(found));
        let message = format!("expected `{expected_shown}`, found `{found_shown}`");
        let mut diagnostic = Diagnostic::error(codes::TYPE_MISMATCH, message.clone(), span, message);
        if let Some((span, label)) = reason {
            diagnostic = diagnostic.with_label(span, label);
        }
        let help = match (expected, found) {
            (Ty::FLOAT, Ty::INT) => Some("convert it with `float(...)`".to_string()),
            (Ty::INT, Ty::FLOAT) => Some("convert it with `int(...)`, which drops the fraction".to_string()),
            (Ty::STR, Ty::INT | Ty::FLOAT | Ty::BOOL) => {
                Some("convert it with `str(...)`, or use an f-string".to_string())
            }
            (_, Ty::NONE) => {
                let shown = self.display(expected);
                Some(format!("only optional types can hold `None`; declare it as `{shown}?`"))
            }
            _ if self.types.option_inner(found) == Some(expected) => {
                Some("this value may be `None`; use `.unwrap()` or `.unwrap_or(default)`".to_string())
            }
            _ => None,
        };
        if let Some(help) = help {
            diagnostic = diagnostic.with_help(help);
        }
        self.error(diagnostic);
    }

    pub(super) fn expr(&mut self, cx: &mut FnCx, expr: &ast::Expr, expected: Option<Ty>) -> hir::Expr {
        let span = expr.span;
        match &expr.kind {
            ast::ExprKind::Int(value) => {
                if expected == Some(Ty::FLOAT) {
                    return mk(K::Float(*value as f64), Ty::FLOAT, span);
                }
                match i64::try_from(*value) {
                    Ok(value) => mk(K::Int(value), Ty::INT, span),
                    Err(_) => {
                        self.error(Diagnostic::error(
                            codes::INT_TOO_LARGE,
                            "integer literal is too large for `int`",
                            span,
                            "the largest `int` is 9223372036854775807",
                        ));
                        error_expr(span)
                    }
                }
            }
            ast::ExprKind::Float(value) => mk(K::Float(*value), Ty::FLOAT, span),
            ast::ExprKind::Str(value) => mk(K::Str(value.clone()), Ty::STR, span),
            ast::ExprKind::Bool(value) => mk(K::Bool(*value), Ty::BOOL, span),
            ast::ExprKind::None => mk(K::None, Ty::NONE, span),
            ast::ExprKind::FString(parts) => self.fstring(cx, parts, span),
            ast::ExprKind::Name(name) => self.name_expr(cx, name, span),
            ast::ExprKind::Call { callee, args } => self.call(cx, callee, args, span),
            ast::ExprKind::Attr { object, name } => self.attr(cx, object, name),
            ast::ExprKind::Index { object, index } => self.index(cx, object, index, span),
            ast::ExprKind::Slice { object, start, stop } => {
                self.slice(cx, object, start.as_deref(), stop.as_deref(), span)
            }
            ast::ExprKind::List(items) => self.list_literal(cx, items, span, expected),
            ast::ExprKind::Dict(pairs) => self.dict_literal(cx, pairs, span, expected),
            ast::ExprKind::Tuple(items) => self.tuple_literal(cx, items, span, expected),
            ast::ExprKind::Unary { op, operand } => self.unary(cx, *op, operand, span, expected),
            ast::ExprKind::Binary { op, lhs, rhs } => self.binary(cx, *op, lhs, rhs, span),
            ast::ExprKind::IfElse { cond, then, otherwise } => self.if_else(cx, cond, then, otherwise, span, expected),
        }
    }

    // ---- names --------------------------------------------------------------------------

    pub(super) fn name_expr(&mut self, cx: &mut FnCx, name: &str, span: Span) -> hir::Expr {
        if let Some(&local) = cx.scope.get(name) {
            cx.info[local.0].read = true;
            let ty = cx.locals[local.0].ty;
            if !cx.is_assigned(local) {
                let decl_span = cx.info[local.0].decl_span;
                self.error(
                    Diagnostic::error(codes::USED_BEFORE_ASSIGNED, format!("`{name}` might not be assigned here"), span, "may be unassigned here")
                        .with_label(decl_span, "assigned here, but not on every path to the use")
                        .with_help("make sure every path assigns it first, for example by giving it a value before the `if` or loop"),
                );
                return error_expr(span);
            }
            return mk(K::Local(local), ty, span);
        }
        match self.globals.get(name).map(|(global, _)| *global) {
            Some(Global::Const(index)) => {
                return match &self.consts[index].value {
                    Some(value) => hir::Expr { span, ..value.clone() },
                    None => error_expr(span),
                };
            }
            Some(Global::Function(_)) => {
                self.error(
                    Diagnostic::error(
                        codes::NOT_SUPPORTED,
                        "using a function as a value is not supported yet",
                        span,
                        "functions can only be called for now",
                    )
                    .with_help(format!("to call it, write `{name}(...)`")),
                );
                return error_expr(span);
            }
            Some(Global::Struct(_)) => {
                self.error(
                    Diagnostic::error(codes::NOT_SUPPORTED, format!("`{name}` is a type, not a value"), span, "a type")
                        .with_help(format!("create a value with `{name}(...)`")),
                );
                return error_expr(span);
            }
            None => {}
        }
        if BUILTIN_FUNCTIONS.contains(&name) {
            self.error(
                Diagnostic::error(
                    codes::NOT_SUPPORTED,
                    "using a function as a value is not supported yet",
                    span,
                    "functions can only be called for now",
                )
                .with_help(format!("to call it, write `{name}(...)`")),
            );
            return error_expr(span);
        }
        if let Some(&assigned) = cx.first_assignment.get(name) {
            self.error(
                Diagnostic::error(
                    codes::USED_BEFORE_ASSIGNED,
                    format!("`{name}` is used before it is assigned"),
                    span,
                    "used here",
                )
                .with_label(assigned, "first assigned here"),
            );
            return error_expr(span);
        }
        self.unknown_name(cx, name, span);
        error_expr(span)
    }

    fn unknown_name(&mut self, cx: &FnCx, name: &str, span: Span) {
        let help = match name {
            "true" => Some("did you mean `True`?".to_string()),
            "false" => Some("did you mean `False`?".to_string()),
            "null" | "nil" | "none" => Some("did you mean `None`?".to_string()),
            "self" => Some("`self` is only available in methods that take it as their first parameter".to_string()),
            "pass" => Some("there is no `pass` statement; a block needs at least one real statement".to_string()),
            planned if PLANNED_FUNCTIONS.contains(&planned) => Some(format!("`{planned}` is not available yet")),
            _ => {
                let candidates = cx
                    .scope
                    .keys()
                    .map(String::as_str)
                    .chain(self.globals.keys().map(String::as_str))
                    .chain(BUILTIN_FUNCTIONS.iter().copied());
                super::closest(name, candidates).map(|s| format!("did you mean `{s}`?"))
            }
        };
        let mut diagnostic =
            Diagnostic::error(codes::UNKNOWN_NAME, format!("unknown name `{name}`"), span, "not defined");
        if let Some(help) = help {
            diagnostic = diagnostic.with_help(help);
        }
        self.error(diagnostic);
    }

    // ---- calls --------------------------------------------------------------------------

    fn check_all(&mut self, cx: &mut FnCx, args: &[ast::Arg]) {
        for arg in args {
            self.expr(cx, &arg.value, None);
        }
    }

    fn call(&mut self, cx: &mut FnCx, callee: &ast::Expr, args: &[ast::Arg], span: Span) -> hir::Expr {
        match &callee.kind {
            ast::ExprKind::Name(name) if !cx.scope.contains_key(name) => {
                match self.globals.get(name).map(|(global, _)| *global) {
                    Some(Global::Function(id)) => {
                        let args = self.call_args(cx, id, args, span);
                        let ret = self.signatures[id.0].ret;
                        return mk(K::Call { func: id, args }, ret, span);
                    }
                    Some(Global::Struct(id)) => return self.construct(cx, id, args, span),
                    Some(Global::Const(_)) => {
                        self.error(Diagnostic::error(
                            codes::NOT_A_FUNCTION,
                            format!("`{name}` is a constant, not a function"),
                            callee.span,
                            "not callable",
                        ));
                        self.check_all(cx, args);
                        return error_expr(span);
                    }
                    None => {}
                }
                if BUILTIN_FUNCTIONS.contains(&name.as_str()) {
                    return self.builtin_function(cx, name, args, span);
                }
                self.check_all(cx, args);
                self.unknown_name(cx, name, callee.span);
                error_expr(span)
            }
            ast::ExprKind::Name(name) => {
                let local = cx.scope[name];
                let (ty, decl_span) = (cx.locals[local.0].ty, cx.info[local.0].decl_span);
                let shown = self.display(ty);
                self.error(
                    Diagnostic::error(
                        codes::NOT_A_FUNCTION,
                        format!("`{name}` is not a function"),
                        callee.span,
                        format!("has type `{shown}`"),
                    )
                    .with_label(decl_span, format!("`{name}` is declared here")),
                );
                self.check_all(cx, args);
                error_expr(span)
            }
            ast::ExprKind::Attr { object, name } => {
                if let ast::ExprKind::Name(type_name) = &object.kind
                    && !cx.scope.contains_key(type_name)
                {
                    if let Some(&(Global::Struct(id), _)) = self.globals.get(type_name) {
                        return self.static_call(cx, id, name, args, span);
                    }
                    if matches!(type_name.as_str(), "int" | "float" | "str" | "bool" | "list" | "dict") {
                        self.error(
                            Diagnostic::error(
                                codes::NOT_SUPPORTED,
                                format!("`{type_name}.{}` is not supported yet", name.name),
                                callee.span,
                                "not supported yet",
                            )
                            .with_help("parsing text into numbers arrives with `Result` in milestone 3"),
                        );
                        self.check_all(cx, args);
                        return error_expr(span);
                    }
                }
                let receiver = self.expr(cx, object, None);
                self.method_call(cx, receiver, name, args, span)
            }
            _ => {
                self.error(Diagnostic::error(
                    codes::NOT_SUPPORTED,
                    "only calls to a function by name are supported yet",
                    callee.span,
                    "not a function name",
                ));
                self.check_all(cx, args);
                error_expr(span)
            }
        }
    }

    /// Arguments of a call to a user function or method (not counting `self`).
    fn call_args(&mut self, cx: &mut FnCx, id: hir::FuncId, args: &[ast::Arg], span: Span) -> Vec<hir::Expr> {
        let signature = &self.signatures[id.0];
        let (name, def_span, params) = (signature.name.clone(), signature.name_span, signature.params.clone());
        if let Some(arg) = args.iter().find(|a| a.name.is_some()) {
            let span = arg.name.as_ref().unwrap().span;
            self.error(
                Diagnostic::error(
                    codes::NOT_SUPPORTED,
                    "keyword arguments for functions are not supported yet",
                    span,
                    "not supported yet",
                )
                .with_help("they work when constructing structs; functions get them in milestone 4"),
            );
            self.check_all(cx, args);
            return Vec::new();
        }
        if args.len() != params.len() {
            let given = args.len();
            let verb = if given == 1 { "was" } else { "were" };
            self.error(
                Diagnostic::error(
                    codes::WRONG_ARG_COUNT,
                    format!("`{name}` takes {} but {given} {verb} given", super::plural(params.len(), "argument")),
                    span,
                    format!("expected {}", super::plural(params.len(), "argument")),
                )
                .with_label(def_span, format!("`{name}` is defined here")),
            );
            self.check_all(cx, args);
            return Vec::new();
        }
        args.iter()
            .zip(&params)
            .map(|(arg, param)| {
                let reason = Some((param.hint_span, format!("parameter `{}` is declared here", param.name)));
                self.expect(cx, &arg.value, param.ty, reason)
            })
            .collect()
    }

    fn static_call(
        &mut self,
        cx: &mut FnCx,
        id: crate::types::StructId,
        name: &ast::Ident,
        args: &[ast::Arg],
        span: Span,
    ) -> hir::Expr {
        let struct_name = self.structs[id.0].name.clone();
        let Some(&func) = self.methods.get(&(id, name.name.clone())) else {
            self.error(Diagnostic::error(
                codes::NO_SUCH_MEMBER,
                format!("`{struct_name}` has no method `{}`", name.name),
                name.span,
                "no such method",
            ));
            self.check_all(cx, args);
            return error_expr(span);
        };
        if self.signatures[func.0].receiver.is_some() {
            self.error(
                Diagnostic::error(
                    codes::NOT_A_FUNCTION,
                    format!("`{struct_name}.{}` needs a `{struct_name}` value", name.name),
                    name.span,
                    "takes `self`",
                )
                .with_help(format!("call it on a value: `value.{}(...)`", name.name)),
            );
            self.check_all(cx, args);
            return error_expr(span);
        }
        let args = self.call_args(cx, func, args, span);
        let ret = self.signatures[func.0].ret;
        mk(K::Call { func, args }, ret, span)
    }

    pub(super) fn method_call(
        &mut self,
        cx: &mut FnCx,
        receiver: hir::Expr,
        name: &ast::Ident,
        args: &[ast::Arg],
        span: Span,
    ) -> hir::Expr {
        match self.types.kind(receiver.ty).clone() {
            TyKind::Struct(id) => {
                let struct_name = self.structs[id.0].name.clone();
                let Some(&func) = self.methods.get(&(id, name.name.clone())) else {
                    let is_field = self.structs[id.0].fields.iter().any(|f| f.name == name.name);
                    let mut diagnostic = Diagnostic::error(
                        codes::NO_SUCH_MEMBER,
                        format!("`{struct_name}` has no method `{}`", name.name),
                        name.span,
                        if is_field { "this is a field" } else { "no such method" },
                    );
                    if is_field {
                        diagnostic =
                            diagnostic.with_help(format!("fields cannot be called; read it with `.{}`", name.name));
                    } else {
                        let candidates: Vec<String> =
                            self.methods.keys().filter(|(owner, _)| *owner == id).map(|(_, m)| m.clone()).collect();
                        if let Some(s) = super::closest(&name.name, candidates.iter().map(String::as_str)) {
                            diagnostic = diagnostic.with_help(format!("did you mean `{s}`?"));
                        }
                    }
                    self.error(diagnostic);
                    self.check_all(cx, args);
                    return error_expr(span);
                };
                if self.signatures[func.0].receiver.is_none() {
                    self.error(
                        Diagnostic::error(
                            codes::NOT_A_FUNCTION,
                            format!("`{}` does not take `self`", name.name),
                            name.span,
                            "called on a value",
                        )
                        .with_help(format!("call it on the type: `{struct_name}.{}(...)`", name.name)),
                    );
                    self.check_all(cx, args);
                    return error_expr(span);
                }
                let mut all = vec![receiver];
                all.extend(self.call_args(cx, func, args, span));
                let ret = self.signatures[func.0].ret;
                mk(K::Call { func, args: all }, ret, span)
            }
            TyKind::Error => {
                self.check_all(cx, args);
                error_expr(span)
            }
            TyKind::Str | TyKind::List(_) | TyKind::Dict(..) | TyKind::Option(_) => {
                self.builtin_method(cx, receiver, name, args, span)
            }
            _ => {
                let shown = self.display(receiver.ty);
                self.error(Diagnostic::error(
                    codes::NO_SUCH_MEMBER,
                    format!("`{shown}` has no method `{}`", name.name),
                    name.span,
                    "no such method",
                ));
                self.check_all(cx, args);
                error_expr(span)
            }
        }
    }

    /// `Point(1, 2)` or `Point(x=1, y=2)`. Fields left out take their defaults.
    fn construct(&mut self, cx: &mut FnCx, id: crate::types::StructId, args: &[ast::Arg], span: Span) -> hir::Expr {
        let info = &self.structs[id.0];
        let (struct_name, struct_span, struct_ty, fields) =
            (info.name.clone(), info.span, info.ty, info.fields.clone());
        let mut values: Vec<Option<hir::Expr>> = vec![None; fields.len()];
        let mut positional = 0;
        let mut seen_keyword = false;
        for arg in args {
            let index = match &arg.name {
                None => {
                    if seen_keyword {
                        self.error(Diagnostic::error(
                            codes::UNEXPECTED_TOKEN,
                            "a positional argument cannot follow a keyword argument",
                            arg.value.span,
                            "positional",
                        ));
                        self.expr(cx, &arg.value, None);
                        continue;
                    }
                    positional += 1;
                    if positional > fields.len() {
                        self.error(
                            Diagnostic::error(
                                codes::WRONG_ARG_COUNT,
                                format!(
                                    "`{struct_name}` has {}, but more values were given",
                                    super::plural(fields.len(), "field")
                                ),
                                arg.value.span,
                                "extra value",
                            )
                            .with_label(struct_span, format!("`{struct_name}` is defined here")),
                        );
                        self.expr(cx, &arg.value, None);
                        continue;
                    }
                    positional - 1
                }
                Some(name) => {
                    seen_keyword = true;
                    let Some(index) = fields.iter().position(|f| f.name == name.name) else {
                        let mut diagnostic = Diagnostic::error(
                            codes::NO_SUCH_MEMBER,
                            format!("`{struct_name}` has no field `{}`", name.name),
                            name.span,
                            "no such field",
                        )
                        .with_label(struct_span, format!("`{struct_name}` is defined here"));
                        if let Some(s) = super::closest(&name.name, fields.iter().map(|f| f.name.as_str())) {
                            diagnostic = diagnostic.with_help(format!("did you mean `{s}`?"));
                        }
                        self.error(diagnostic);
                        self.expr(cx, &arg.value, None);
                        continue;
                    };
                    if values[index].is_some() {
                        self.error(Diagnostic::error(
                            codes::DUPLICATE_DEFINITION,
                            format!("field `{}` is given more than once", name.name),
                            name.span,
                            "given again here",
                        ));
                        self.expr(cx, &arg.value, None);
                        continue;
                    }
                    index
                }
            };
            let field = &fields[index];
            let reason = Some((field.span, format!("field `{}` is declared here", field.name)));
            values[index] = Some(self.expect(cx, &arg.value, field.ty, reason));
        }

        let missing: Vec<&str> = fields
            .iter()
            .zip(&values)
            .filter(|(field, value)| value.is_none() && field.default.is_none())
            .map(|(field, _)| field.name.as_str())
            .collect();
        if !missing.is_empty() {
            let list = missing.iter().map(|m| format!("`{m}`")).collect::<Vec<_>>().join(", ");
            let word = if missing.len() == 1 { "field" } else { "fields" };
            self.error(
                Diagnostic::error(
                    codes::MISSING_FIELD,
                    format!("missing {word} {list} in `{struct_name}(...)`"),
                    span,
                    format!("missing {list}"),
                )
                .with_label(struct_span, format!("`{struct_name}` is defined here")),
            );
            return error_expr(span);
        }
        let fields: Vec<hir::Expr> = values
            .into_iter()
            .zip(fields)
            .map(|(value, field)| value.unwrap_or_else(|| hir::Expr { span, ..field.default.expect("checked above") }))
            .collect();
        mk(K::StructNew { id, fields }, struct_ty, span)
    }

    // ---- fields, indexing, slicing ------------------------------------------------------

    fn attr(&mut self, cx: &mut FnCx, object: &ast::Expr, name: &ast::Ident) -> hir::Expr {
        let span = object.span.to(name.span);
        if let ast::ExprKind::Name(type_name) = &object.kind
            && !cx.scope.contains_key(type_name)
            && let Some(&(Global::Struct(_), _)) = self.globals.get(type_name)
        {
            self.error(
                Diagnostic::error(
                    codes::NOT_SUPPORTED,
                    format!("`{type_name}.{}` needs a value, not the type", name.name),
                    span,
                    "used on the type",
                )
                .with_help(format!(
                    "read fields from a value of type `{type_name}`; call static methods as `{type_name}.{}(...)`",
                    name.name
                )),
            );
            return error_expr(span);
        }
        let object = self.expr(cx, object, None);
        if object.ty != Ty::ERROR && self.struct_of(object.ty).is_none() {
            let shown = self.display(object.ty);
            let is_method = self.is_builtin_method(object.ty, &name.name);
            let mut diagnostic = Diagnostic::error(
                codes::NO_SUCH_MEMBER,
                format!("`{shown}` has no field `{}`", name.name),
                name.span,
                if is_method { "this is a method" } else { "no such field" },
            );
            if is_method {
                diagnostic = diagnostic.with_help(format!("call it: `.{}()`", name.name));
            }
            self.error(diagnostic);
            return error_expr(span);
        }
        match self.field_of(&object, name) {
            Some((index, ty, _)) => mk(K::Field { object: Box::new(object), index }, ty, span),
            None => error_expr(span),
        }
    }

    fn index(&mut self, cx: &mut FnCx, object: &ast::Expr, index: &ast::Expr, span: Span) -> hir::Expr {
        let container = self.expr(cx, object, None);
        match self.types.kind(container.ty).clone() {
            TyKind::List(elem) => {
                let index = self.expect(cx, index, Ty::INT, None);
                mk(K::Index { container: Box::new(container), index: Box::new(index) }, elem, span)
            }
            TyKind::Dict(key, value) => {
                let index = self.expect(cx, index, key, None);
                mk(K::Index { container: Box::new(container), index: Box::new(index) }, value, span)
            }
            TyKind::Tuple(items) => {
                let literal = match &index.kind {
                    ast::ExprKind::Int(v) => i64::try_from(*v).ok(),
                    ast::ExprKind::Unary { op: ast::UnaryOp::Neg, operand } => match operand.kind {
                        ast::ExprKind::Int(v) => i64::try_from(v).ok().map(|v| -v),
                        _ => None,
                    },
                    _ => None,
                };
                let Some(n) = literal else {
                    self.error(
                        Diagnostic::error(
                            codes::BAD_INDEX,
                            "a tuple index must be a number written in the code",
                            index.span,
                            "not a literal",
                        )
                        .with_help(
                            "tuple elements can have different types, so the index must be known when compiling",
                        ),
                    );
                    return error_expr(span);
                };
                let len = items.len() as i64;
                let position = if n < 0 { n + len } else { n };
                if position < 0 || position >= len {
                    let shown = self.display(container.ty);
                    self.error(Diagnostic::error(
                        codes::BAD_INDEX,
                        format!("index {n} is out of range for `{shown}`"),
                        index.span,
                        format!("the tuple has {}", super::plural(items.len(), "element")),
                    ));
                    return error_expr(span);
                }
                let ty = items[position as usize];
                mk(K::TupleGet { tuple: Box::new(container), index: position as usize }, ty, span)
            }
            TyKind::Error => {
                self.expr(cx, index, None);
                error_expr(span)
            }
            TyKind::Str => {
                self.error(
                    Diagnostic::error(
                        codes::BAD_INDEX,
                        "strings cannot be indexed by position",
                        span,
                        "not supported for `str`",
                    )
                    .with_help("slice it with byte offsets, `text[i:i + 1]`, or loop over its characters"),
                );
                error_expr(span)
            }
            _ => {
                let shown = self.display(container.ty);
                self.error(Diagnostic::error(
                    codes::BAD_INDEX,
                    format!("`{shown}` cannot be indexed"),
                    span,
                    format!("has type `{shown}`"),
                ));
                error_expr(span)
            }
        }
    }

    fn slice(
        &mut self,
        cx: &mut FnCx,
        object: &ast::Expr,
        start: Option<&ast::Expr>,
        stop: Option<&ast::Expr>,
        span: Span,
    ) -> hir::Expr {
        let container = self.expr(cx, object, None);
        let start = start.map(|e| Box::new(self.expect(cx, e, Ty::INT, None)));
        let stop = stop.map(|e| Box::new(self.expect(cx, e, Ty::INT, None)));
        match self.types.kind(container.ty) {
            TyKind::List(_) | TyKind::Str => {
                let ty = container.ty;
                mk(K::Slice { container: Box::new(container), start, stop }, ty, span)
            }
            TyKind::Error => error_expr(span),
            _ => {
                let shown = self.display(container.ty);
                self.error(Diagnostic::error(
                    codes::BAD_INDEX,
                    format!("`{shown}` cannot be sliced"),
                    span,
                    "only lists and strings can be sliced",
                ));
                error_expr(span)
            }
        }
    }

    // ---- literals -----------------------------------------------------------------------

    fn list_literal(&mut self, cx: &mut FnCx, items: &[ast::Expr], span: Span, expected: Option<Ty>) -> hir::Expr {
        if items.is_empty() && expected.is_some_and(|t| self.types.contains_error(t)) {
            return error_expr(span);
        }
        let expected_elem = expected.and_then(|t| match self.types.kind(t) {
            TyKind::List(elem) => Some(*elem),
            _ => None,
        });
        if items.is_empty() {
            return match expected_elem {
                Some(elem) => {
                    let ty = self.types.list(elem);
                    mk(K::List(Vec::new()), ty, span)
                }
                None => {
                    self.error(
                        Diagnostic::error(
                            codes::CANNOT_INFER,
                            "cannot infer the type of an empty list",
                            span,
                            "element type unknown",
                        )
                        .with_help("add a type hint, for example `items: list[int] = []`"),
                    );
                    error_expr(span)
                }
            };
        }
        let (elem, first) = match expected_elem {
            Some(elem) => (elem, None),
            None => {
                let first = self.expr(cx, &items[0], None);
                if first.ty == Ty::NONE {
                    self.error(
                        Diagnostic::error(
                            codes::CANNOT_INFER,
                            "cannot infer the element type of this list",
                            span,
                            "starts with `None`",
                        )
                        .with_help("add a type hint, for example `items: list[int?] = [None, 1]`"),
                    );
                    return error_expr(span);
                }
                (first.ty, Some(first))
            }
        };
        let reason =
            first.as_ref().map(|f| (f.span, format!("the elements are `{}` because of this one", self.display(elem))));
        let mut values: Vec<hir::Expr> = first.into_iter().collect();
        for item in &items[values.len()..] {
            values.push(self.expect(cx, item, elem, reason.clone()));
        }
        let ty = self.types.list(elem);
        mk(K::List(values), ty, span)
    }

    fn dict_literal(
        &mut self,
        cx: &mut FnCx,
        pairs: &[(ast::Expr, ast::Expr)],
        span: Span,
        expected: Option<Ty>,
    ) -> hir::Expr {
        if pairs.is_empty() && expected.is_some_and(|t| self.types.contains_error(t)) {
            return error_expr(span);
        }
        let expected_types = expected.and_then(|t| match self.types.kind(t) {
            TyKind::Dict(key, value) => Some((*key, *value)),
            _ => None,
        });
        let (key_ty, value_ty, mut values) = match (expected_types, pairs.first()) {
            (Some((key, value)), _) => (key, value, Vec::new()),
            (None, None) => {
                self.error(
                    Diagnostic::error(
                        codes::CANNOT_INFER,
                        "cannot infer the type of an empty dict",
                        span,
                        "key and value types unknown",
                    )
                    .with_help("add a type hint, for example `counts: dict[str, int] = {}`"),
                );
                return error_expr(span);
            }
            (None, Some((key, value))) => {
                let key = self.expr(cx, key, None);
                let value = self.expr(cx, value, None);
                if !self.types.is_hashable(key.ty) {
                    self.unhashable(key.ty, key.span);
                    return error_expr(span);
                }
                if value.ty == Ty::NONE {
                    self.error(
                        Diagnostic::error(
                            codes::CANNOT_INFER,
                            "cannot infer the value type of this dict",
                            value.span,
                            "`None`",
                        )
                        .with_help("add a type hint, for example `d: dict[str, int?] = {...}`"),
                    );
                    return error_expr(span);
                }
                (key.ty, value.ty, vec![(key, value)])
            }
        };
        for (key, value) in &pairs[values.len()..] {
            let key = self.expect(cx, key, key_ty, None);
            let value = self.expect(cx, value, value_ty, None);
            values.push((key, value));
        }
        let ty = self.types.dict(key_ty, value_ty);
        mk(K::Dict(values), ty, span)
    }

    fn tuple_literal(&mut self, cx: &mut FnCx, items: &[ast::Expr], span: Span, expected: Option<Ty>) -> hir::Expr {
        let expected_items = expected.and_then(|t| match self.types.kind(t) {
            TyKind::Tuple(types) if types.len() == items.len() => Some(types.clone()),
            _ => None,
        });
        let values: Vec<hir::Expr> = match expected_items {
            Some(types) => items.iter().zip(types).map(|(item, ty)| self.expect(cx, item, ty, None)).collect(),
            None => items.iter().map(|item| self.expr(cx, item, None)).collect(),
        };
        if let Some(none) = values.iter().find(|v| v.ty == Ty::NONE) {
            self.error(
                Diagnostic::error(
                    codes::CANNOT_INFER,
                    "a tuple element cannot be `None` without a type hint",
                    none.span,
                    "`None`",
                )
                .with_help("add a type hint, for example `pair: tuple[int, str?] = (1, None)`"),
            );
            return error_expr(span);
        }
        let ty = self.types.tuple(values.iter().map(|v| v.ty).collect());
        mk(K::Tuple(values), ty, span)
    }

    fn fstring(&mut self, cx: &mut FnCx, parts: &[ast::FStringPart], span: Span) -> hir::Expr {
        let mut out = Vec::new();
        for part in parts {
            match part {
                ast::FStringPart::Literal(text) => out.push(hir::FPart::Literal(text.clone())),
                ast::FStringPart::Expr { expr, spec } => {
                    let value = self.expr(cx, expr, None);
                    if value.ty == Ty::ERROR {
                        continue;
                    }
                    if !self.types.is_displayable(value.ty) {
                        self.not_displayable(value.ty, value.span);
                        continue;
                    }
                    let spec = match spec {
                        None => None,
                        Some((text, spec_span)) => {
                            let kind = match value.ty {
                                Ty::INT => Some(Kind::Int),
                                Ty::FLOAT => Some(Kind::Float),
                                Ty::STR => Some(Kind::Str),
                                _ => None,
                            };
                            let Some(kind) = kind else {
                                let shown = self.display(value.ty);
                                self.error(Diagnostic::error(
                                    codes::BAD_FORMAT_SPEC,
                                    format!("a format spec cannot be applied to `{shown}`"),
                                    *spec_span,
                                    "format specs apply to `int`, `float` and `str`",
                                ));
                                continue;
                            };
                            if let Err(message) = Spec::parse(text).and_then(|s| s.check(kind)) {
                                self.error(
                                    Diagnostic::error(codes::BAD_FORMAT_SPEC, format!("invalid format spec `{text}`"), *spec_span, message)
                                        .with_help("the spec follows Python's format mini-language, for example `.2f`, `>8` or `,`"),
                                );
                                continue;
                            }
                            Some(text.clone())
                        }
                    };
                    out.push(hir::FPart::Value { expr: value, spec });
                }
            }
        }
        mk(K::FString(out), Ty::STR, span)
    }

    pub(super) fn not_displayable(&mut self, ty: Ty, span: Span) {
        let shown = self.display(ty);
        self.error(
            Diagnostic::error(codes::NOT_SUPPORTED, format!("a `{shown}` cannot be printed yet"), span, format!("has type `{shown}`"))
                .with_help("printing structs needs the `Display` trait, which arrives with traits in milestone 3; print its fields instead"),
        );
    }

    // ---- operators ----------------------------------------------------------------------

    fn unary(
        &mut self,
        cx: &mut FnCx,
        op: ast::UnaryOp,
        operand: &ast::Expr,
        span: Span,
        expected: Option<Ty>,
    ) -> hir::Expr {
        // `-9223372036854775808` is the one literal whose magnitude does not fit in `int`.
        if let (ast::UnaryOp::Neg, ast::ExprKind::Int(value)) = (op, &operand.kind)
            && *value == i64::MIN.unsigned_abs()
            && expected != Some(Ty::FLOAT)
        {
            return mk(K::Int(i64::MIN), Ty::INT, span);
        }
        let value = self.expr(cx, operand, expected.filter(|t| *t == Ty::FLOAT));
        if value.ty == Ty::ERROR {
            return error_expr(span);
        }
        match (op, value.ty) {
            (ast::UnaryOp::Neg, Ty::INT) => match value.kind {
                K::Int(v) => mk(K::Int(v.wrapping_neg()), Ty::INT, span),
                _ => mk(K::Unary { op: hir::UnaryOp::IntNeg, operand: Box::new(value) }, Ty::INT, span),
            },
            (ast::UnaryOp::Neg, Ty::FLOAT) => match value.kind {
                K::Float(v) => mk(K::Float(-v), Ty::FLOAT, span),
                _ => mk(K::Unary { op: hir::UnaryOp::FloatNeg, operand: Box::new(value) }, Ty::FLOAT, span),
            },
            (ast::UnaryOp::Plus, Ty::INT | Ty::FLOAT) => value,
            (ast::UnaryOp::Not, Ty::BOOL) => {
                mk(K::Unary { op: hir::UnaryOp::Not, operand: Box::new(value) }, Ty::BOOL, span)
            }
            (ast::UnaryOp::Invert, Ty::INT) => {
                mk(K::Unary { op: hir::UnaryOp::IntInvert, operand: Box::new(value) }, Ty::INT, span)
            }
            (ast::UnaryOp::Not, found) => {
                let shown = self.display(found);
                self.error(
                    Diagnostic::error(
                        codes::BAD_OPERANDS,
                        format!("`not` needs a `bool`, found `{shown}`"),
                        operand.span,
                        format!("has type `{shown}`"),
                    )
                    .with_help("values are not true or false by themselves; compare explicitly, for example `x == 0`"),
                );
                error_expr(span)
            }
            (op, found) => {
                let shown = self.display(found);
                self.error(Diagnostic::error(
                    codes::BAD_OPERANDS,
                    format!("cannot apply unary `{}` to `{shown}`", op.symbol()),
                    span,
                    format!("operand has type `{shown}`"),
                ));
                error_expr(span)
            }
        }
    }

    fn binary(&mut self, cx: &mut FnCx, op: ast::BinaryOp, lhs: &ast::Expr, rhs: &ast::Expr, span: Span) -> hir::Expr {
        use ast::BinaryOp as A;
        match op {
            A::And | A::Or => {
                let l = self.bool_operand(cx, op, lhs);
                let r = self.bool_operand(cx, op, rhs);
                let kind =
                    if op == A::And { K::And(Box::new(l), Box::new(r)) } else { K::Or(Box::new(l), Box::new(r)) };
                mk(kind, Ty::BOOL, span)
            }
            A::Is | A::IsNot => self.identity(cx, op == A::IsNot, lhs, rhs, span),
            A::Eq | A::NotEq if lhs.is_none_literal() || rhs.is_none_literal() => {
                self.identity(cx, op == A::NotEq, lhs, rhs, span)
            }
            A::In | A::NotIn => self.membership(cx, op == A::NotIn, lhs, rhs, span),
            _ => {
                // An integer literal on the left adapts to the type on the right: `2 * x`.
                let (l, r) = if lhs.is_int_literal() && !rhs.is_int_literal() {
                    let r = self.expr(cx, rhs, None);
                    let l = self.expr(cx, lhs, Some(r.ty));
                    (l, r)
                } else {
                    let l = self.expr(cx, lhs, None);
                    let r = self.expr(cx, rhs, Some(l.ty));
                    (l, r)
                };
                self.typed_binary(op, l, r, span)
            }
        }
    }

    /// For augmented assignment: `current op= value`.
    pub(super) fn binary_with(
        &mut self,
        cx: &mut FnCx,
        op: ast::BinaryOp,
        current: hir::Expr,
        value: &ast::Expr,
        span: Span,
    ) -> hir::Expr {
        let r = self.expr(cx, value, Some(current.ty));
        self.typed_binary(op, current, r, span)
    }

    fn bool_operand(&mut self, cx: &mut FnCx, op: ast::BinaryOp, operand: &ast::Expr) -> hir::Expr {
        let value = self.expr(cx, operand, Some(Ty::BOOL));
        if !self.types.fits(value.ty, Ty::BOOL) {
            let shown = self.display(value.ty);
            self.error(
                Diagnostic::error(
                    codes::BAD_OPERANDS,
                    format!("`{}` needs `bool` operands, found `{shown}`", op.symbol()),
                    operand.span,
                    format!("has type `{shown}`"),
                )
                .with_help(
                    "values are not true or false by themselves; compare explicitly, for example `len(items) > 0`",
                ),
            );
        }
        value
    }

    fn typed_binary(&mut self, op: ast::BinaryOp, l: hir::Expr, r: hir::Expr, span: Span) -> hir::Expr {
        use ast::BinaryOp as A;
        if self.types.contains_error(l.ty) || self.types.contains_error(r.ty) {
            return error_expr(span);
        }
        let comparison = matches!(op, A::Eq | A::NotEq | A::Lt | A::Le | A::Gt | A::Ge);
        if comparison {
            // Comparing `x: int?` with `5` wraps the plain side.
            let (l, r) = match (self.types.option_inner(l.ty), self.types.option_inner(r.ty)) {
                (Some(inner), None) if inner == r.ty => {
                    let ty = l.ty;
                    (l, self.coerce(r, ty, None))
                }
                (None, Some(inner)) if inner == l.ty => {
                    let ty = r.ty;
                    (self.coerce(l, ty, None), r)
                }
                _ => (l, r),
            };
            if l.ty != r.ty {
                return self.bad_operands(op, &l, &r, span);
            }
            let ordering = !matches!(op, A::Eq | A::NotEq);
            let allowed = if ordering { self.types.is_orderable(l.ty) } else { self.types.is_equatable(l.ty) };
            if !allowed {
                let shown = self.display(l.ty);
                let help = if self.struct_of(l.ty).is_some() {
                    if ordering {
                        "ordering structs needs the `Ord` trait, which arrives with traits"
                    } else {
                        "`==` on structs needs the `Eq` trait, which arrives with traits; use `is` to check whether two values are the same object"
                    }
                } else if ordering {
                    "`<` and friends work on numbers, strings and booleans"
                } else {
                    "comparing dicts with `==` is not supported yet"
                };
                self.error(
                    Diagnostic::error(
                        codes::BAD_OPERANDS,
                        format!("cannot compare `{shown}` values with `{}`", op.symbol()),
                        span,
                        format!("both sides are `{shown}`"),
                    )
                    .with_help(help),
                );
                return error_expr(span);
            }
            let hir_op = match op {
                A::Eq => B::Eq,
                A::NotEq => B::NotEq,
                A::Lt => B::Lt,
                A::Le => B::Le,
                A::Gt => B::Gt,
                _ => B::Ge,
            };
            return mk(K::Binary { op: hir_op, lhs: Box::new(l), rhs: Box::new(r) }, Ty::BOOL, span);
        }

        let (hir_op, ty) = match (op, self.types.kind(l.ty), self.types.kind(r.ty)) {
            (A::Add, TyKind::Int, TyKind::Int) => (B::IntAdd, Ty::INT),
            (A::Sub, TyKind::Int, TyKind::Int) => (B::IntSub, Ty::INT),
            (A::Mul, TyKind::Int, TyKind::Int) => (B::IntMul, Ty::INT),
            (A::Div, TyKind::Int, TyKind::Int) => (B::IntTrueDiv, Ty::FLOAT),
            (A::FloorDiv, TyKind::Int, TyKind::Int) => (B::IntFloorDiv, Ty::INT),
            (A::Mod, TyKind::Int, TyKind::Int) => (B::IntMod, Ty::INT),
            (A::Pow, TyKind::Int, TyKind::Int) => (B::IntPow, Ty::INT),
            (A::BitAnd, TyKind::Int, TyKind::Int) => (B::IntAnd, Ty::INT),
            (A::BitOr, TyKind::Int, TyKind::Int) => (B::IntOr, Ty::INT),
            (A::BitXor, TyKind::Int, TyKind::Int) => (B::IntXor, Ty::INT),
            (A::Shl, TyKind::Int, TyKind::Int) => (B::IntShl, Ty::INT),
            (A::Shr, TyKind::Int, TyKind::Int) => (B::IntShr, Ty::INT),
            (A::Add, TyKind::Float, TyKind::Float) => (B::FloatAdd, Ty::FLOAT),
            (A::Sub, TyKind::Float, TyKind::Float) => (B::FloatSub, Ty::FLOAT),
            (A::Mul, TyKind::Float, TyKind::Float) => (B::FloatMul, Ty::FLOAT),
            (A::Div, TyKind::Float, TyKind::Float) => (B::FloatDiv, Ty::FLOAT),
            (A::FloorDiv, TyKind::Float, TyKind::Float) => (B::FloatFloorDiv, Ty::FLOAT),
            (A::Mod, TyKind::Float, TyKind::Float) => (B::FloatMod, Ty::FLOAT),
            (A::Pow, TyKind::Float, TyKind::Float) => (B::FloatPow, Ty::FLOAT),
            (A::Add, TyKind::Str, TyKind::Str) => (B::StrConcat, Ty::STR),
            (A::Add, TyKind::List(_), TyKind::List(_)) if l.ty == r.ty => (B::ListConcat, l.ty),
            _ => return self.bad_operands(op, &l, &r, span),
        };
        mk(K::Binary { op: hir_op, lhs: Box::new(l), rhs: Box::new(r) }, ty, span)
    }

    fn bad_operands(&mut self, op: ast::BinaryOp, l: &hir::Expr, r: &hir::Expr, span: Span) -> hir::Expr {
        let (ls, rs) = (self.display(l.ty), self.display(r.ty));
        let symbol = op.symbol();
        let help = match (l.ty, r.ty) {
            (Ty::INT, Ty::FLOAT) | (Ty::FLOAT, Ty::INT) => {
                "numbers are not converted implicitly; convert one side with `float(...)` or `int(...)`"
            }
            (Ty::STR, _) | (_, Ty::STR) => {
                "strings only combine with strings; use an f-string or `str(...)` to build text"
            }
            _ => "both sides must have types that support this operator",
        };
        self.error(
            Diagnostic::error(
                codes::BAD_OPERANDS,
                format!("cannot use `{symbol}` on `{ls}` and `{rs}`"),
                span,
                format!("`{ls}` {symbol} `{rs}`"),
            )
            .with_help(help),
        );
        error_expr(span)
    }

    /// `x is None`, `x is not None`, `x == None`, and identity of references: `a is b`.
    fn identity(&mut self, cx: &mut FnCx, negate: bool, lhs: &ast::Expr, rhs: &ast::Expr, span: Span) -> hir::Expr {
        let op = if negate { B::IsNot } else { B::Is };
        if lhs.is_none_literal() || rhs.is_none_literal() {
            let other = if rhs.is_none_literal() { lhs } else { rhs };
            let value = self.expr(cx, other, None);
            if value.ty == Ty::ERROR {
                return error_expr(span);
            }
            if self.types.option_inner(value.ty).is_none() {
                let shown = self.display(value.ty);
                self.error(
                    Diagnostic::error(
                        codes::BAD_OPERANDS,
                        format!("a `{shown}` can never be `None`"),
                        value.span,
                        format!("has type `{shown}`"),
                    )
                    .with_help("only optional types such as `int?` can hold `None`"),
                );
                return error_expr(span);
            }
            let none = mk(K::None, value.ty, span);
            return mk(K::Binary { op, lhs: Box::new(value), rhs: Box::new(none) }, Ty::BOOL, span);
        }
        let l = self.expr(cx, lhs, None);
        let r = self.expr(cx, rhs, Some(l.ty));
        if self.types.contains_error(l.ty) || self.types.contains_error(r.ty) {
            return error_expr(span);
        }
        if l.ty != r.ty || !self.types.is_reference(l.ty) {
            let (ls, rs) = (self.display(l.ty), self.display(r.ty));
            self.error(
                Diagnostic::error(
                    codes::BAD_OPERANDS,
                    format!("cannot use `is` on `{ls}` and `{rs}`"),
                    span,
                    "not the same kind of object",
                )
                .with_help(
                    "`is` checks whether two structs, lists or dicts are the same object; use `==` to compare values",
                ),
            );
            return error_expr(span);
        }
        mk(K::Binary { op, lhs: Box::new(l), rhs: Box::new(r) }, Ty::BOOL, span)
    }

    fn membership(
        &mut self,
        cx: &mut FnCx,
        negate: bool,
        needle: &ast::Expr,
        container: &ast::Expr,
        span: Span,
    ) -> hir::Expr {
        let haystack = self.expr(cx, container, None);
        let needle = match self.types.kind(haystack.ty).clone() {
            TyKind::Str => self.expect(cx, needle, Ty::STR, None),
            TyKind::List(elem) => {
                if !self.types.is_equatable(elem) {
                    let shown = self.display(elem);
                    self.error(
                        Diagnostic::error(
                            codes::BAD_OPERANDS,
                            format!("cannot search a list of `{shown}` with `in`"),
                            span,
                            "elements cannot be compared",
                        )
                        .with_help("`in` compares elements with `==`, which structs get with traits"),
                    );
                    return error_expr(span);
                }
                self.expect(cx, needle, elem, None)
            }
            TyKind::Dict(key, _) => self.expect(cx, needle, key, None),
            TyKind::Error => return error_expr(span),
            _ => {
                let shown = self.display(haystack.ty);
                self.error(Diagnostic::error(
                    codes::BAD_OPERANDS,
                    format!("`in` needs a `str`, list or dict on the right, found `{shown}`"),
                    container.span,
                    format!("has type `{shown}`"),
                ));
                return error_expr(span);
            }
        };
        let op = if negate { B::NotIn } else { B::In };
        mk(K::Binary { op, lhs: Box::new(needle), rhs: Box::new(haystack) }, Ty::BOOL, span)
    }

    fn if_else(
        &mut self,
        cx: &mut FnCx,
        cond: &ast::Expr,
        then: &ast::Expr,
        otherwise: &ast::Expr,
        span: Span,
        expected: Option<Ty>,
    ) -> hir::Expr {
        let cond = self.condition(cx, cond);
        let then = self.expr(cx, then, expected);
        let otherwise = self.expr(cx, otherwise, Some(then.ty).filter(|t| *t != Ty::NONE).or(expected));
        let ty = if self.types.fits(otherwise.ty, then.ty) {
            then.ty
        } else if let Some(e) = expected.filter(|e| self.coercible(then.ty, *e) && self.coercible(otherwise.ty, *e)) {
            e
        } else if then.ty == Ty::NONE {
            self.types.option(otherwise.ty)
        } else if otherwise.ty == Ty::NONE {
            self.types.option(then.ty)
        } else {
            let (a, b) = (self.display(then.ty), self.display(otherwise.ty));
            self.error(Diagnostic::error(
                codes::TYPE_MISMATCH,
                format!("the two branches have different types, `{a}` and `{b}`"),
                span,
                "both branches must have the same type",
            ));
            return error_expr(span);
        };
        let then = self.coerce(then, ty, None);
        let otherwise = self.coerce(otherwise, ty, None);
        mk(K::IfElse { cond: Box::new(cond), then: Box::new(then), otherwise: Box::new(otherwise) }, ty, span)
    }

    fn coercible(&self, from: Ty, to: Ty) -> bool {
        self.types.fits(from, to)
            || self.types.option_inner(to).is_some_and(|inner| from == Ty::NONE || self.types.fits(from, inner))
    }
}
