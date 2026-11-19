//! Expressions.
//!
//! Checking is bidirectional: `expected` carries the type the context wants, when known. It is a
//! hint, used to give `[]`, `{}`, `None`, `Ok`, `Err` and integer literals their types;
//! [`Checker::expect`] and [`Checker::coerce`] then enforce it, applying the only implicit
//! conversions the language has: `T` to `T?`, `None` to any optional type, `T` to `Result[T, E]`
//! (as `Ok`), and a struct or enum to a trait it implements.

use super::{BUILTIN_CONSTRUCTORS, BUILTIN_FUNCTIONS, Checker, FnCx, Global, Reason, error_expr};
use crate::hir::{self, BinaryOp as B, ExprKind as K};
use crate::types::{EnumId, TraitId, Ty, TyKind};
use lang_diag::{Diagnostic, Span, codes};
use lang_format::{Kind, Spec};
use lang_syntax::ast;

/// Built-in functions from the design that are not implemented yet.
const PLANNED_FUNCTIONS: &[&str] = &["open", "input", "map", "filter", "reversed", "set"];

fn mk(kind: hir::ExprKind, ty: Ty, span: Span) -> hir::Expr {
    hir::Expr { kind, ty, span }
}

impl Checker<'_, '_> {
    pub(super) fn expect(&mut self, cx: &mut FnCx, expr: &ast::Expr, expected: Ty, reason: Reason) -> hir::Expr {
        let checked = self.expr(cx, expr, Some(expected));
        self.coerce(checked, expected, reason)
    }

    /// Accept `value` where `expected` is required, applying the implicit conversions (to an
    /// optional, to `Ok`, to a trait object), or report a mismatch.
    pub(super) fn coerce(&mut self, value: hir::Expr, expected: Ty, reason: Reason) -> hir::Expr {
        if let Some(conversion) = self.conversion(value.ty, expected) {
            return self.convert(value, conversion, expected);
        }
        let value = match self.json_value_of(value, expected) {
            Ok(converted) => return converted,
            Err(value) => value,
        };
        if let Some(inner) = self.types.option_inner(expected) {
            if matches!(value.kind, K::None) && value.ty == Ty::NONE {
                return hir::Expr { ty: expected, ..value };
            }
            if let Some(conversion) = self.conversion(value.ty, inner) {
                let span = value.span;
                let inner_value = self.convert(value, conversion, inner);
                return mk(K::Some(Box::new(inner_value)), expected, span);
            }
        }
        if let Some((ok, _)) = self.types.result_parts(expected)
            && let Some(conversion) = self.conversion(value.ty, ok)
        {
            let span = value.span;
            let ok_value = self.convert(value, conversion, ok);
            return mk(K::Ok(Box::new(ok_value)), expected, span);
        }
        self.mismatch(value.span, expected, value.ty, reason);
        value
    }

    /// How a value of type `from` can be used as `to` without changing what it is: as it is, as a
    /// trait object, or as a trait object of a required trait.
    pub(super) fn conversion(&self, from: Ty, to: Ty) -> Option<hir::Conversion> {
        if self.types.fits(from, to) {
            return Some(hir::Conversion::Identity);
        }
        let TyKind::Dyn(target) = *self.types.kind(to) else { return None };
        match *self.types.kind(from) {
            TyKind::Struct(_) | TyKind::Enum(_) if self.implements(from, target) => {
                Some(hir::Conversion::ToDyn(target))
            }
            TyKind::Dyn(source) => self.upcast_path(source, target).map(hir::Conversion::Upcast),
            TyKind::SelfType(source) if self.trait_closure(source).contains(&target) => {
                Some(hir::Conversion::ToDyn(target))
            }
            _ => None,
        }
    }

    pub(super) fn convert(&mut self, value: hir::Expr, conversion: hir::Conversion, to: Ty) -> hir::Expr {
        let span = value.span;
        match conversion {
            hir::Conversion::Identity => value,
            hir::Conversion::Upcast(path) if path.is_empty() => hir::Expr { ty: to, ..value },
            hir::Conversion::Upcast(path) => mk(K::Upcast { value: Box::new(value), path }, to, span),
            hir::Conversion::ToDyn(trait_id) => mk(K::ToDyn { value: Box::new(value), trait_id }, to, span),
        }
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
        if let Some(value) = self.json_literal(cx, expr, expected) {
            return value;
        }
        match &expr.kind {
            ast::ExprKind::Int(value) => {
                if expected == Some(Ty::FLOAT) {
                    return mk(K::Float(*value as f64), Ty::FLOAT, span);
                }
                if let Some(ty) = expected
                    && let Some(kind) = self.types.sized_kind(ty)
                {
                    if kind.is_float() {
                        return mk(K::Float(*value as f64), ty, span);
                    }
                    let bits = kind.bits();
                    let max = match (kind.is_signed(), bits) {
                        (_, 64) => u64::MAX,
                        (true, b) => (1u64 << (b - 1)) - 1,
                        (false, b) => (1u64 << b) - 1,
                    };
                    if *value > max {
                        self.error(Diagnostic::error(
                            codes::INT_TOO_LARGE,
                            format!("integer literal is too large for `{}`", kind.name()),
                            span,
                            format!("the largest `{}` is {max}", kind.name()),
                        ));
                        return error_expr(span);
                    }
                    return mk(K::Int(*value as i64), ty, span);
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
            ast::ExprKind::Float(value) => {
                match expected.filter(|t| self.types.sized_kind(*t).is_some_and(|k| k.is_float())) {
                    Some(f32_ty) => mk(K::Float(*value), f32_ty, span),
                    None => mk(K::Float(*value), Ty::FLOAT, span),
                }
            }
            ast::ExprKind::Str(value) => mk(K::Str(value.clone()), Ty::STR, span),
            ast::ExprKind::Bool(value) => mk(K::Bool(*value), Ty::BOOL, span),
            ast::ExprKind::None => mk(K::None, Ty::NONE, span),
            ast::ExprKind::FString(parts) => self.fstring(cx, parts, span),
            ast::ExprKind::Name(name) => self.name_value(cx, name, span, expected),
            ast::ExprKind::Call { callee, args } => self.call(cx, callee, args, span, expected),
            ast::ExprKind::Attr { object, name } => self.attr(cx, object, name, expected),
            ast::ExprKind::Index { object, index } => self.index(cx, object, index, span),
            ast::ExprKind::Slice { object, start, stop } => {
                self.slice(cx, object, start.as_deref(), stop.as_deref(), span)
            }
            ast::ExprKind::List(items) | ast::ExprKind::Tuple(items)
                if expected.is_some_and(|t| matches!(self.types.kind(t), TyKind::FrozenList(_))) =>
            {
                // A literal written where a `tuple[T, ...]` is expected is built frozen.
                let frozen = expected.expect("checked");
                let TyKind::FrozenList(elem) = *self.types.kind(frozen) else { unreachable!() };
                let list_ty = self.types.list(elem);
                let list = self.list_literal(cx, items, span, Some(list_ty));
                if list.ty == list_ty { hir::Expr { ty: frozen, ..list } } else { list }
            }
            ast::ExprKind::Dict(pairs)
                if expected.is_some_and(|t| matches!(self.types.kind(t), TyKind::FrozenDict(..))) =>
            {
                let frozen = expected.expect("checked");
                let TyKind::FrozenDict(k, v) = *self.types.kind(frozen) else { unreachable!() };
                let dict_ty = self.types.dict(k, v);
                let dict = self.dict_literal(cx, pairs, span, Some(dict_ty));
                if dict.ty == dict_ty { hir::Expr { ty: frozen, ..dict } } else { dict }
            }
            ast::ExprKind::List(items) => self.list_literal(cx, items, span, expected),
            ast::ExprKind::Dict(pairs) => self.dict_literal(cx, pairs, span, expected),
            ast::ExprKind::Tuple(items) => self.tuple_literal(cx, items, span, expected),
            ast::ExprKind::Unary { op, operand } => self.unary(cx, *op, operand, span, expected),
            ast::ExprKind::Binary { op, lhs, rhs } => self.binary(cx, *op, lhs, rhs, span),
            ast::ExprKind::IfElse { cond, then, otherwise } => self.if_else(cx, cond, then, otherwise, span, expected),
            ast::ExprKind::Try(inner) => self.try_expr(cx, inner, span),
            ast::ExprKind::Lambda { params, body } => self.lambda(cx, params, body, expected, span),
            ast::ExprKind::Spawn(call) => self.spawn_expr(cx, call, span),
            ast::ExprKind::ListComp { element, clauses } => {
                self.comprehension(cx, element, None, clauses, span, expected)
            }
            ast::ExprKind::DictComp { key, value, clauses } => {
                self.comprehension(cx, key, Some(value), clauses, span, expected)
            }
        }
    }

    /// `value?`
    fn try_expr(&mut self, cx: &mut FnCx, inner: &ast::Expr, span: Span) -> hir::Expr {
        let value = self.expr(cx, inner, None);
        if self.types.contains_error(value.ty) || self.types.contains_error(cx.ret) {
            return error_expr(span);
        }
        let shown_ret = self.display(cx.ret);
        match self.types.kind(value.ty).clone() {
            TyKind::Result(ok, err) => {
                let Some((_, ret_err)) = self.types.result_parts(cx.ret) else {
                    self.error(
                        Diagnostic::error(codes::BAD_TRY, "`?` passes an error to the caller, but this function does not return a `Result`", span, "error has nowhere to go")
                            .with_help(format!("declare the function as returning a `Result` (it returns `{shown_ret}`), or handle the error here with `match`")),
                    );
                    return error_expr(span);
                };
                let Some(conversion) = self.conversion(err, ret_err) else {
                    let (from, to) = (self.display(err), self.display(ret_err));
                    self.error(
                        Diagnostic::error(codes::BAD_TRY, format!("`?` cannot pass on a `{from}` error from a function whose errors are `{to}`"), span, format!("the error is `{from}`"))
                            .with_help("declare the function as returning `Result[T]`, whose errors can be any type implementing `Error`"),
                    );
                    return error_expr(span);
                };
                mk(K::Try { value: Box::new(value), conversion }, ok, span)
            }
            TyKind::Option(inner_ty) => {
                if self.types.option_inner(cx.ret).is_none() {
                    self.error(
                        Diagnostic::error(codes::BAD_TRY, "`?` on an optional value returns `None`, but this function does not return an optional", span, "`None` has nowhere to go")
                            .with_help(format!("declare the function as returning an optional type (it returns `{shown_ret}`), or check with `is None`")),
                    );
                    return error_expr(span);
                }
                mk(K::Try { value: Box::new(value), conversion: hir::Conversion::Identity }, inner_ty, span)
            }
            _ => {
                let shown = self.display(value.ty);
                self.error(Diagnostic::error(
                    codes::BAD_TRY,
                    format!("`?` needs a `Result` or an optional value, found `{shown}`"),
                    span,
                    format!("has type `{shown}`"),
                ));
                error_expr(span)
            }
        }
    }

    // ---- names --------------------------------------------------------------------------

    pub(super) fn name_expr(&mut self, cx: &mut FnCx, name: &str, span: Span) -> hir::Expr {
        self.name_value(cx, name, span, None)
    }

    fn name_value(&mut self, cx: &mut FnCx, name: &str, span: Span, expected: Option<Ty>) -> hir::Expr {
        if let Some(local) = self.lookup_local(cx, name) {
            cx.info[local.0].read = true;
            let ty = cx.locals[local.0].ty;
            if cx.expired.contains(&local) {
                self.error(
                    Diagnostic::error(
                        codes::GUARD_ESCAPE,
                        format!("`{name}` was bound by a `with` block on a lock, and cannot be used after it"),
                        span,
                        "used after the lock was released",
                    )
                    .with_help("do the work inside the `with` block, or copy what you need out of it there"),
                );
                return error_expr(span);
            }
            if cx.locals[local.0].capture.is_some() && !self.shareable(ty) && Self::captures_guard(cx, name) {
                self.error(Diagnostic::error(
                    codes::GUARD_ESCAPE,
                    format!("`{name}` is protected by a lock and cannot be captured by a nested function"),
                    span,
                    "captured here",
                ));
                return error_expr(span);
            }
            if !cx.is_assigned(local) {
                let decl_span = cx.info[local.0].decl_span;
                self.error(
                    Diagnostic::error(codes::USED_BEFORE_ASSIGNED, format!("`{name}` might not be assigned here"), span, "may be unassigned here")
                        .with_label(decl_span, "assigned here, but not on every path to the use")
                        .with_help("make sure every path assigns it first, for example by giving it a value before the `if` or loop"),
                );
                return error_expr(span);
            }
            let value = mk(K::Local(local), ty, span);
            if cx.narrowed.contains(&local)
                && let Some(inner) = self.types.option_inner(ty)
            {
                // Known not to be `None` here.
                return mk(K::Builtin { op: hir::Builtin::OptUnwrap, args: vec![value] }, inner, span);
            }
            return value;
        }
        if let Some(global) = self.globals.get(name).map(|(global, _)| *global) {
            return self.global_value(cx, global, name, span, expected);
        }
        match self.lookup_variant(name, span) {
            Some(Ok((enum_id, index))) => return self.variant_value(cx, enum_id, index, None, span),
            Some(Err(())) => return error_expr(span),
            None => {}
        }
        if let Some((template, variant)) = self.lookup_generic_variant(name, span) {
            return self.generic_variant(cx, template, variant, None, None, span, expected);
        }
        if BUILTIN_FUNCTIONS.contains(&name)
            && let Some(TyKind::Func(params, _)) = expected.map(|t| self.types.kind(t).clone())
        {
            return self.builtin_value(cx, name, params, span);
        }
        if BUILTIN_FUNCTIONS.contains(&name) || BUILTIN_CONSTRUCTORS.contains(&name) {
            self.error(
                Diagnostic::error(
                    codes::CANNOT_INFER,
                    format!("cannot tell which `{name}` this is"),
                    span,
                    "a built-in function used as a value",
                )
                .with_help(format!(
                    "built-in functions can be values where a function type is expected; otherwise wrap it: `lambda x: {name}(x)`"
                )),
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

    /// A variant used by its bare name: the enum and index if the name is unique, an error (already
    /// reported) if several enums have it, `None` if no enum does.
    fn lookup_variant(&mut self, name: &str, span: Span) -> Option<Result<(EnumId, usize), ()>> {
        let mut owners = self.variant_names.get(name)?.clone();
        owners.retain(|(e, _)| self.enum_in_scope(Global::Enum(*e)));
        if owners.is_empty() {
            return None;
        }
        if let [single] = owners.as_slice() {
            return Some(Ok(*single));
        }
        let names: Vec<String> = owners.iter().map(|(e, _)| format!("`{}.{name}`", self.enums[e.0].name)).collect();
        self.error(
            Diagnostic::error(
                codes::UNKNOWN_NAME,
                format!("`{name}` is a variant of more than one enum"),
                span,
                "ambiguous",
            )
            .with_help(format!("say which one: {}", names.join(" or "))),
        );
        Some(Err(()))
    }

    /// A variant of a generic enum used by its bare name, when only one generic enum has it.
    fn lookup_generic_variant(&mut self, name: &str, span: Span) -> Option<(usize, usize)> {
        let mut owners = self.generic_variant_names.get(name)?.clone();
        owners.retain(|(t, _)| self.enum_in_scope(Global::GenericEnum(*t)));
        if owners.is_empty() {
            return None;
        }
        if let [single] = owners.as_slice() {
            return Some(*single);
        }
        let names: Vec<String> =
            owners.iter().map(|(t, _)| format!("`{}.{name}`", self.generic_types[*t].name)).collect();
        self.error(
            Diagnostic::error(
                codes::UNKNOWN_NAME,
                format!("`{name}` is a variant of more than one enum"),
                span,
                "ambiguous",
            )
            .with_help(format!("say which one: {}", names.join(" or "))),
        );
        None
    }

    /// A value of an enum variant. `args` is `None` when the variant is named without a call.
    pub(super) fn variant_value(
        &mut self,
        cx: &mut FnCx,
        enum_id: EnumId,
        index: usize,
        args: Option<&[ast::Arg]>,
        span: Span,
    ) -> hir::Expr {
        let info = &self.enums[enum_id.0];
        let (enum_name, ty) = (info.name.clone(), info.ty);
        let variant = &info.variants[index];
        let (variant_name, variant_span, fields) = (variant.name.clone(), variant.span, variant.fields.clone());
        let full = format!("{enum_name}.{variant_name}");
        match args {
            None if fields.is_empty() => mk(K::Variant { index, fields: Vec::new() }, ty, span),
            None => {
                self.error(
                    Diagnostic::error(
                        codes::MISSING_FIELD,
                        format!("variant `{full}` needs values for its fields"),
                        span,
                        "fields missing",
                    )
                    .with_label(variant_span, format!("`{variant_name}` is declared here")),
                );
                error_expr(span)
            }
            Some(args) if fields.is_empty() => {
                if !args.is_empty() {
                    self.error(Diagnostic::error(
                        codes::WRONG_ARG_COUNT,
                        format!("variant `{full}` has no fields"),
                        span,
                        format!("write `{full}` without `(...)`"),
                    ));
                }
                mk(K::Variant { index, fields: Vec::new() }, ty, span)
            }
            Some(args) => match self.field_values(cx, &full, variant_span, &fields, args, span) {
                Some(values) => mk(K::Variant { index, fields: values }, ty, span),
                None => error_expr(span),
            },
        }
    }

    fn unknown_name(&mut self, cx: &FnCx, name: &str, span: Span) {
        let help = match name {
            "true" => Some("did you mean `True`?".to_string()),
            "false" => Some("did you mean `False`?".to_string()),
            "null" | "nil" | "none" => Some("did you mean `None`?".to_string()),
            "self" => Some("`self` is only available in methods that take it as their first parameter".to_string()),
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

    fn call(
        &mut self,
        cx: &mut FnCx,
        callee: &ast::Expr,
        args: &[ast::Arg],
        span: Span,
        expected: Option<Ty>,
    ) -> hir::Expr {
        // `module.name(...)`
        if let ast::ExprKind::Attr { object, name } = &callee.kind
            && let Some(global) = self.member_global(cx, object, name)
        {
            let shown = format!("{}.{}", Self::dotted(object).unwrap_or_default(), name.name);
            return self.call_global(cx, global, &shown, callee.span, args, span, expected);
        }
        match &callee.kind {
            ast::ExprKind::Name(name) if !cx.sees(name) => {
                if let Some(global) = self.globals.get(name).map(|(global, _)| *global) {
                    return self.call_global(cx, global, name, callee.span, args, span, expected);
                }
                if name.starts_with("__json_") && self.in_json_module() {
                    return self.json_intrinsic(cx, name, args, span, expected);
                }
                if name == "__http_serve" && self.in_http_module() {
                    return self.http_serve_intrinsic(cx, args, span);
                }
                if super::concurrency::SYNC_TYPES.contains(&name.as_str()) && name != "Task" {
                    return self.sync_constructor(cx, name, None, args, span, expected);
                }
                if BUILTIN_FUNCTIONS.contains(&name.as_str()) {
                    return self.builtin_function(cx, name, args, span);
                }
                if BUILTIN_CONSTRUCTORS.contains(&name.as_str()) {
                    return self.constructor(cx, name, args, span, expected);
                }
                match self.lookup_variant(name, callee.span) {
                    Some(Ok((enum_id, index))) => return self.variant_value(cx, enum_id, index, Some(args), span),
                    Some(Err(())) => {
                        self.check_all(cx, args);
                        return error_expr(span);
                    }
                    None => {}
                }
                if let Some((template, variant)) = self.lookup_generic_variant(name, callee.span) {
                    return self.generic_variant(cx, template, variant, None, Some(args), span, expected);
                }
                self.check_all(cx, args);
                self.unknown_name(cx, name, callee.span);
                error_expr(span)
            }
            ast::ExprKind::Name(name) => {
                let value = self.name_expr(cx, name, callee.span);
                if value.ty != Ty::ERROR && !matches!(self.types.kind(value.ty), TyKind::Func(..)) {
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
                    return error_expr(span);
                }
                self.call_value(cx, value, args, span)
            }
            ast::ExprKind::Index { object, index } if matches!(&object.kind, ast::ExprKind::Name(n) if (n == "Channel" || n == "Mutex") && !cx.sees(n) && !self.globals.contains_key(n)) =>
            {
                let ast::ExprKind::Name(name) = &object.kind else { unreachable!() };
                match self.type_args_of(index) {
                    Some(types) => self.sync_constructor(cx, name, Some(types), args, span, expected),
                    None => {
                        self.check_all_quietly(cx, args);
                        error_expr(span)
                    }
                }
            }
            ast::ExprKind::Index { object, index } if self.generic_name(cx, object).is_some() => {
                // `Box[int](...)` or `f[int](...)`: explicit type arguments.
                match self.generic_name(cx, object) {
                    Some(Global::GenericStruct(template)) => {
                        let instance = self.explicit_instance(template, index, callee.span);
                        match instance {
                            Some(ty) => self.construct_generic(cx, template, Some(ty), args, span, expected),
                            None => {
                                self.check_all_quietly(cx, args);
                                error_expr(span)
                            }
                        }
                    }
                    Some(Global::GenericFunction(f)) => match self.type_args_of(index) {
                        Some(types) => self.call_generic_explicit(cx, f, types, args, span),
                        None => {
                            self.check_all_quietly(cx, args);
                            error_expr(span)
                        }
                    },
                    _ => {
                        self.error(Diagnostic::error(
                            codes::NOT_A_FUNCTION,
                            "an enum cannot be called; create a value with one of its variants",
                            callee.span,
                            "an enum",
                        ));
                        self.check_all_quietly(cx, args);
                        error_expr(span)
                    }
                }
            }
            ast::ExprKind::Attr { object, name } if matches!(&object.kind, ast::ExprKind::Index { object: inner, .. } if self.generic_name(cx, inner).is_some()) =>
            {
                // `Stack[int].new()` and `Tree[int].Node(...)`.
                let ast::ExprKind::Index { object: inner, index } = &object.kind else { unreachable!() };
                let template = match self.generic_name(cx, inner) {
                    Some(Global::GenericStruct(t) | Global::GenericEnum(t)) => t,
                    _ => {
                        self.error(Diagnostic::error(
                            codes::NOT_A_FUNCTION,
                            "not a generic type",
                            inner.span,
                            "not a type",
                        ));
                        self.check_all_quietly(cx, args);
                        return error_expr(span);
                    }
                };
                let Some(ty) = self.explicit_instance(template, index, object.span) else {
                    self.check_all_quietly(cx, args);
                    return error_expr(span);
                };
                if let Some(id) = self.enum_of(ty)
                    && let Some(variant) = self.enums[id.0].variants.iter().position(|v| v.name == name.name)
                {
                    return self.variant_value(cx, id, variant, Some(args), span);
                }
                self.static_call(cx, ty, name, args, span)
            }
            ast::ExprKind::Attr { object, name } => {
                if let Some(type_name) = Self::dotted(object)
                    && !cx.sees(type_name.split('.').next().unwrap_or_default())
                {
                    let type_name = type_name.as_str();
                    let global = self.path_global(cx, object);
                    match global {
                        Some(Global::GenericStruct(template)) => {
                            return self.generic_static_call(cx, template, None, name, args, span, expected);
                        }
                        Some(Global::GenericEnum(template)) => {
                            let pattern = self.pattern_of(template);
                            if let Some(id) = self.enum_of(pattern)
                                && let Some(variant) =
                                    self.enums[id.0].variants.iter().position(|v| v.name == name.name)
                            {
                                return self.generic_variant(cx, template, variant, None, Some(args), span, expected);
                            }
                            return self.generic_static_call(cx, template, None, name, args, span, expected);
                        }
                        Some(Global::GenericFunction(_)) => {}
                        _ => {}
                    }
                    match global {
                        Some(Global::Struct(id)) => {
                            let ty = self.structs[id.0].ty;
                            return self.static_call(cx, ty, name, args, span);
                        }
                        Some(Global::Enum(id)) => {
                            if let Some(index) = self.enums[id.0].variants.iter().position(|v| v.name == name.name) {
                                return self.variant_value(cx, id, index, Some(args), span);
                            }
                            let ty = self.enums[id.0].ty;
                            return self.static_call(cx, ty, name, args, span);
                        }
                        Some(Global::Trait(id)) => return self.trait_call(cx, id, name, args, span),
                        _ => {}
                    }
                    if matches!(type_name, "int" | "float") && name.name == "parse" {
                        return self.parse_call(cx, type_name, args, span);
                    }
                    if matches!(type_name, "int" | "float" | "str" | "bool" | "list" | "dict") {
                        self.error(
                            Diagnostic::error(
                                codes::NOT_SUPPORTED,
                                format!("`{type_name}.{}` does not exist", name.name),
                                callee.span,
                                "no such function",
                            )
                            .with_help(
                                "the functions on built-in types so far are `int.parse(text)` and `float.parse(text)`",
                            ),
                        );
                        self.check_all(cx, args);
                        return error_expr(span);
                    }
                }
                let mut receiver = self.expr(cx, object, None);
                // `x.unwrap()` still works on a narrowed `x`: it applies to the optional itself.
                if let K::Builtin { op: hir::Builtin::OptUnwrap, args: inner } = &receiver.kind
                    && matches!(inner[0].kind, K::Local(_))
                    && matches!(name.name.as_str(), "unwrap" | "unwrap_or" | "is_some" | "is_none")
                    && !self.has_method(receiver.ty, &name.name)
                {
                    receiver = inner[0].clone();
                }
                self.method_call(cx, receiver, name, args, span)
            }
            _ => {
                let value = self.expr(cx, callee, None);
                self.call_value(cx, value, args, span)
            }
        }
    }

    /// A call to something named by a global: a function, a struct (construction), or an error.
    #[allow(clippy::too_many_arguments)]
    fn call_global(
        &mut self,
        cx: &mut FnCx,
        global: Global,
        name: &str,
        callee_span: Span,
        args: &[ast::Arg],
        span: Span,
        expected: Option<Ty>,
    ) -> hir::Expr {
        let callee = ast::Expr { kind: ast::ExprKind::None, span: callee_span };
        match global {
            // An alias of a struct type constructs the struct.
            Global::TypeAlias(index) => {
                let ty = self.alias_type_of(index, callee_span);
                match self.struct_of(ty) {
                    Some(id) => self.construct(cx, id, args, span),
                    None => {
                        self.error(Diagnostic::error(
                            codes::NOT_A_FUNCTION,
                            format!("`{name}` is a type, not a function"),
                            callee_span,
                            "not callable",
                        ));
                        self.check_all(cx, args);
                        error_expr(span)
                    }
                }
            }
            Global::Function(id) => {
                let signature = &self.signatures[id.0];
                let (fname, def_span, params, ret) =
                    (signature.name.clone(), signature.name_span, signature.params.clone(), signature.ret);
                let args = self.call_args(cx, &fname, def_span, &params, args, span);
                mk(K::Call { func: id, args }, ret, span)
            }
            Global::GenericFunction(index) => self.call_generic(cx, index, args, span, expected),
            Global::Struct(id) => self.construct(cx, id, args, span),
            Global::GenericStruct(index) => self.construct_generic(cx, index, None, args, span, expected),
            Global::Enum(_) | Global::GenericEnum(_) => {
                self.error(
                    Diagnostic::error(
                        codes::NOT_A_FUNCTION,
                        format!("`{name}` is an enum; create a value with one of its variants"),
                        callee.span,
                        "an enum",
                    )
                    .with_help(format!("for example `{name}.Variant(...)`")),
                );
                self.check_all(cx, args);
                error_expr(span)
            }
            Global::Trait(_) | Global::Const(_) => {
                let kind = self.global_kind(name).unwrap_or("not a function");
                self.error(Diagnostic::error(
                    codes::NOT_A_FUNCTION,
                    format!("`{name}` is {kind}, not a function"),
                    callee.span,
                    "not callable",
                ));
                self.check_all(cx, args);
                error_expr(span)
            }
            Global::Module(_) => {
                self.error(Diagnostic::error(
                    codes::NOT_A_FUNCTION,
                    format!("`{name}` is a module, not a function"),
                    callee.span,
                    "a module",
                ));
                self.check_all(cx, args);
                error_expr(span)
            }
            Global::Missing => {
                self.check_all_quietly(cx, args);
                error_expr(span)
            }
        }
    }

    fn has_method(&self, ty: Ty, name: &str) -> bool {
        self.is_builtin_method(ty, name)
            || self.inherent.contains_key(&(ty, name.to_string()))
            || self.trait_methods.contains_key(&(ty, name.to_string()))
    }

    /// The generic declaration an expression names, if it is a bare name of one.
    fn generic_name(&self, cx: &FnCx, expr: &ast::Expr) -> Option<Global> {
        let global = match &expr.kind {
            ast::ExprKind::Name(name) if !cx.sees(name) => self.globals.get(name).map(|(g, _)| *g),
            ast::ExprKind::Attr { object, name } => {
                let path = Self::dotted(object)?;
                if cx.sees(path.split('.').next().unwrap_or_default()) {
                    return None;
                }
                match self.globals.get(&path) {
                    Some((Global::Module(m), _)) => {
                        self.exports[*m].get(&name.name).filter(|(_, _, public)| *public).map(|(g, _, _)| *g)
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        match global {
            Some(g @ (Global::GenericStruct(_) | Global::GenericEnum(_) | Global::GenericFunction(_))) => Some(g),
            _ => None,
        }
    }

    /// `Some(x)`, `Ok(x)` and `Err(e)`.
    fn constructor(
        &mut self,
        cx: &mut FnCx,
        name: &str,
        args: &[ast::Arg],
        span: Span,
        expected: Option<Ty>,
    ) -> hir::Expr {
        let [arg] = args else {
            self.error(Diagnostic::error(
                codes::WRONG_ARG_COUNT,
                format!("`{name}` takes one value"),
                span,
                format!("write `{name}(value)`"),
            ));
            self.check_all(cx, args);
            return error_expr(span);
        };
        if arg.name.is_some() {
            self.error(Diagnostic::error(
                codes::NOT_SUPPORTED,
                format!("`{name}` takes no keyword arguments"),
                span,
                "remove the name",
            ));
        }
        let expected_option = expected.and_then(|t| self.types.option_inner(t).map(|inner| (t, inner)));
        let expected_result = expected.and_then(|t| self.types.result_parts(t).map(|parts| (t, parts)));
        match name {
            "Some" => match expected_option {
                Some((ty, inner)) => {
                    let value = self.expect(cx, &arg.value, inner, None);
                    mk(K::Some(Box::new(value)), ty, span)
                }
                None => {
                    let value = self.expr(cx, &arg.value, None);
                    if value.ty == Ty::NONE {
                        self.error(Diagnostic::error(
                            codes::CANNOT_INFER,
                            "`Some(None)` has no useful type",
                            span,
                            "write `None`",
                        ));
                        return error_expr(span);
                    }
                    let ty = self.types.option(value.ty);
                    mk(K::Some(Box::new(value)), ty, span)
                }
            },
            "Ok" => match expected_result {
                Some((ty, (ok, _))) => {
                    let value = self.expect(cx, &arg.value, ok, None);
                    mk(K::Ok(Box::new(value)), ty, span)
                }
                None => {
                    let value = self.expr(cx, &arg.value, None);
                    let error = self.builtin().error;
                    let error_ty = self.types.dyn_(error);
                    let ty = self.types.result(value.ty, error_ty);
                    mk(K::Ok(Box::new(value)), ty, span)
                }
            },
            _ => match expected_result {
                Some((ty, (_, err))) => {
                    let value = self.expr(cx, &arg.value, Some(err));
                    let Some(conversion) = self.conversion(value.ty, err) else {
                        let (found, want) = (self.display(value.ty), self.display(err));
                        let mut diagnostic = Diagnostic::error(
                            codes::TYPE_MISMATCH,
                            format!("expected an error of type `{want}`, found `{found}`"),
                            value.span,
                            format!("has type `{found}`"),
                        );
                        if self.types.kind(err) == &TyKind::Dyn(self.builtin().error) {
                            diagnostic = diagnostic.with_help(format!(
                                "implement `Error` for `{found}` so it can be returned as an error"
                            ));
                        }
                        self.error(diagnostic);
                        return error_expr(span);
                    };
                    let value = self.convert(value, conversion, err);
                    mk(K::Err(Box::new(value)), ty, span)
                }
                None => {
                    self.expr(cx, &arg.value, None);
                    self.error(
                        Diagnostic::error(codes::CANNOT_INFER, "cannot tell what type of `Result` this `Err` belongs to", span, "type unknown")
                            .with_help("use `Err(...)` where a `Result` is expected, such as in `return` from a function declared `-> Result[T]`"),
                    );
                    error_expr(span)
                }
            },
        }
    }

    /// `int.parse(text)` and `float.parse(text)`.
    fn parse_call(&mut self, cx: &mut FnCx, type_name: &str, args: &[ast::Arg], span: Span) -> hir::Expr {
        let [arg] = args else {
            self.error(Diagnostic::error(
                codes::WRONG_ARG_COUNT,
                format!("`{type_name}.parse` takes one string"),
                span,
                "expected one argument",
            ));
            self.check_all(cx, args);
            return error_expr(span);
        };
        let text = self.expect(cx, &arg.value, Ty::STR, None);
        let (op, ok) =
            if type_name == "int" { (hir::Builtin::ParseInt, Ty::INT) } else { (hir::Builtin::ParseFloat, Ty::FLOAT) };
        let error_ty = self.structs[self.parse_error.expect("prelude").0].ty;
        let ty = self.types.result(ok, error_ty);
        mk(K::Builtin { op, args: vec![text] }, ty, span)
    }

    /// Arguments of a call (not counting `self`).
    pub(super) fn call_args(
        &mut self,
        cx: &mut FnCx,
        name: &str,
        def_span: Span,
        params: &[super::ParamSig],
        args: &[ast::Arg],
        span: Span,
    ) -> Vec<hir::Expr> {
        let Some(slots) = self.arrange(name, def_span, params, args, span) else {
            self.check_all(cx, args);
            return Vec::new();
        };
        slots
            .into_iter()
            .zip(params)
            .map(|(slot, param)| match slot {
                Some(value) => {
                    let reason = Self::param_reason(param);
                    self.expect(cx, value, param.ty, reason)
                }
                None => {
                    let default = param.default.clone().unwrap_or_else(|| error_expr(span));
                    hir::Expr { span, ..default }
                }
            })
            .collect()
    }

    fn call_func(
        &mut self,
        cx: &mut FnCx,
        func: hir::FuncId,
        receiver: Option<hir::Expr>,
        args: &[ast::Arg],
        span: Span,
    ) -> hir::Expr {
        self.check_method_visible(func, span);
        let signature = &self.signatures[func.0];
        let (name, def_span, params, ret) =
            (signature.name.clone(), signature.name_span, signature.params.clone(), signature.ret);
        let mut all: Vec<hir::Expr> = receiver.into_iter().collect();
        all.extend(self.call_args(cx, &name, def_span, &params, args, span));
        mk(K::Call { func, args: all }, ret, span)
    }

    /// `Type.method(...)` for a method without `self`.
    pub(super) fn static_call(
        &mut self,
        cx: &mut FnCx,
        owner: Ty,
        name: &ast::Ident,
        args: &[ast::Arg],
        span: Span,
    ) -> hir::Expr {
        let owner_name = self.display(owner);
        let func = match self.inherent.get(&(owner, name.name.clone())) {
            Some(&func) => Some(func),
            None => match self.trait_methods.get(&(owner, name.name.clone())).cloned().as_deref() {
                Some([(trait_id, index)]) => self.impl_of(*trait_id, owner).and_then(|i| self.impls[i].methods[*index]),
                _ => None,
            },
        };
        let Some(func) = func else {
            self.error(Diagnostic::error(
                codes::NO_SUCH_MEMBER,
                format!("`{owner_name}` has no method `{}`", name.name),
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
                    format!("`{owner_name}.{}` needs a `{owner_name}` value", name.name),
                    name.span,
                    "takes `self`",
                )
                .with_help(format!("call it on a value: `value.{}(...)`", name.name)),
            );
            self.check_all(cx, args);
            return error_expr(span);
        }
        self.call_func(cx, func, None, args, span)
    }

    pub(super) fn impl_of(&self, trait_id: TraitId, ty: Ty) -> Option<usize> {
        self.impl_index.get(&(trait_id, ty)).copied()
    }

    /// `Trait.method(value, ...)`: a trait method called with its receiver as the first argument,
    /// for when two traits give a type methods with the same name.
    fn trait_call(
        &mut self,
        cx: &mut FnCx,
        trait_id: TraitId,
        name: &ast::Ident,
        args: &[ast::Arg],
        span: Span,
    ) -> hir::Expr {
        let trait_name = self.traits[trait_id.0].name.clone();
        let Some((declaring, _, index)) = self.find_trait_method(trait_id, &name.name) else {
            self.error(Diagnostic::error(
                codes::NO_SUCH_MEMBER,
                format!("trait `{trait_name}` has no method `{}`", name.name),
                name.span,
                "no such method",
            ));
            self.check_all(cx, args);
            return error_expr(span);
        };
        let Some((first, rest)) = args.split_first() else {
            self.error(Diagnostic::error(
                codes::WRONG_ARG_COUNT,
                format!("`{trait_name}.{}` needs the value to call it on", name.name),
                span,
                "missing the value",
            ));
            return error_expr(span);
        };
        let receiver = self.expr(cx, &first.value, None);
        if !self.implements(receiver.ty, declaring) {
            let shown = self.display(receiver.ty);
            self.error(Diagnostic::error(
                codes::NO_SUCH_MEMBER,
                format!("`{shown}` does not implement `{trait_name}`"),
                first.value.span,
                format!("has type `{shown}`"),
            ));
            self.check_all(cx, rest);
            return error_expr(span);
        }
        self.trait_method_call(cx, receiver, declaring, index, rest, span)
    }

    /// Call trait method `index` of `trait_id` on a receiver known to implement it.
    fn trait_method_call(
        &mut self,
        cx: &mut FnCx,
        receiver: hir::Expr,
        trait_id: TraitId,
        index: usize,
        args: &[ast::Arg],
        span: Span,
    ) -> hir::Expr {
        let ty = receiver.ty;
        if let TyKind::Dyn(_) | TyKind::SelfType(_) | TyKind::Param(_) = *self.types.kind(ty) {
            let path = match *self.types.kind(ty) {
                TyKind::Dyn(source) | TyKind::SelfType(source) => {
                    self.upcast_path(source, trait_id).unwrap_or_default()
                }
                _ => Vec::new(),
            };
            let method = &self.traits[trait_id.0].methods[index];
            let (name, def_span, params, ret, has_self) =
                (method.name.clone(), method.span, method.params.clone(), method.ret, method.has_self);
            if !has_self {
                self.error(Diagnostic::error(
                    codes::NOT_A_FUNCTION,
                    format!("`{name}` does not take `self`"),
                    span,
                    "cannot be called on a value",
                ));
                return error_expr(span);
            }
            let params: Vec<super::ParamSig> =
                params.into_iter().map(|p| super::ParamSig { ty: self.subst_self(p.ty, ty), ..p }).collect();
            let ret = self.subst_self(ret, ty);
            let args = self.call_args(cx, &name, def_span, &params, args, span);
            return mk(K::DynCall { receiver: Box::new(receiver), trait_id, path, method: index, args }, ret, span);
        }
        let Some(impl_index) = self.impl_of(trait_id, ty) else { return error_expr(span) };
        let b = self.builtin();
        if self.impls[impl_index].derived {
            // Derived methods have no function of their own; they are the built-in operations.
            let method_name = self.traits[trait_id.0].methods[index].name.clone();
            let mut values = vec![receiver];
            let params: Vec<super::ParamSig> = self.traits[trait_id.0].methods[index]
                .params
                .clone()
                .into_iter()
                .map(|p| super::ParamSig { ty, ..p })
                .collect();
            values.extend(self.call_args(cx, &method_name, span, &params, args, span));
            return if trait_id == b.eq && values.len() == 2 {
                let rhs = values.pop().unwrap();
                let lhs = values.pop().unwrap();
                mk(K::Binary { op: B::Eq, lhs: Box::new(lhs), rhs: Box::new(rhs) }, Ty::BOOL, span)
            } else if trait_id == b.display {
                mk(K::Builtin { op: hir::Builtin::ToStr, args: values }, Ty::STR, span)
            } else if trait_id == b.hash {
                mk(K::Builtin { op: hir::Builtin::Hash, args: values }, Ty::INT, span)
            } else if trait_id == b.ord {
                mk(K::Builtin { op: hir::Builtin::Cmp, args: values }, Ty::INT, span)
            } else {
                mk(K::Builtin { op: hir::Builtin::Copy, args: values }, ty, span)
            };
        }
        let Some(func) = self.impls[impl_index].methods[index] else {
            // The implementation is missing this method, which was already reported.
            self.check_all(cx, args);
            return error_expr(span);
        };
        if self.signatures[func.0].receiver.is_none() {
            let name = self.traits[trait_id.0].methods[index].name.clone();
            self.error(
                Diagnostic::error(
                    codes::NOT_A_FUNCTION,
                    format!("`{name}` does not take `self`"),
                    span,
                    "called on a value",
                )
                .with_help(format!("call it on the type: `{}.{name}(...)`", self.display(ty))),
            );
            self.check_all(cx, args);
            return error_expr(span);
        }
        self.call_func(cx, func, Some(receiver), args, span)
    }

    pub(super) fn method_call(
        &mut self,
        cx: &mut FnCx,
        receiver: hir::Expr,
        name: &ast::Ident,
        args: &[ast::Arg],
        span: Span,
    ) -> hir::Expr {
        let ty = receiver.ty;
        if name.name == "mount" && self.is_http_server(ty) {
            return self.mount(cx, receiver, args, span);
        }
        match self.types.kind(ty).clone() {
            TyKind::Struct(_) | TyKind::Enum(_) => {
                let type_name = self.display(ty);
                if let Some(&func) = self.inherent.get(&(ty, name.name.clone())) {
                    if self.signatures[func.0].receiver.is_none() {
                        self.error(
                            Diagnostic::error(
                                codes::NOT_A_FUNCTION,
                                format!("`{}` does not take `self`", name.name),
                                name.span,
                                "called on a value",
                            )
                            .with_help(format!("call it on the type: `{type_name}.{}(...)`", name.name)),
                        );
                        self.check_all(cx, args);
                        return error_expr(span);
                    }
                    let call = self.call_func(cx, func, Some(receiver), args, span);
                    if self.is_http_server(ty) {
                        self.check_registration(cx, &name.name, &call);
                    }
                    return call;
                }
                // A field holding a function: `self.handler(request)`.
                if let Some(id) = self.struct_of(ty)
                    && let Some(index) = self.structs[id.0].fields.iter().position(|f| f.name == name.name)
                    && matches!(self.types.kind(self.structs[id.0].fields[index].ty), TyKind::Func(..))
                {
                    let field_ty = self.structs[id.0].fields[index].ty;
                    let callee = mk(K::Field { object: Box::new(receiver), index }, field_ty, name.span);
                    return self.call_value(cx, callee, args, span);
                }
                match self.trait_methods.get(&(ty, name.name.clone())).cloned().as_deref() {
                    Some([(trait_id, index)]) => self.trait_method_call(cx, receiver, *trait_id, *index, args, span),
                    Some(several) if several.len() > 1 => {
                        let names: Vec<String> =
                            several.iter().map(|(t, _)| format!("`{}`", self.traits[t.0].name)).collect();
                        let first = self.traits[several[0].0.0].name.clone();
                        self.error(
                            Diagnostic::error(
                                codes::NO_SUCH_MEMBER,
                                format!("`{}` is a method of more than one trait `{type_name}` implements", name.name),
                                name.span,
                                "ambiguous",
                            )
                            .with_help(format!(
                                "the traits are {}; say which one with `{first}.{}(value, ...)`",
                                names.join(" and "),
                                name.name
                            )),
                        );
                        self.check_all(cx, args);
                        error_expr(span)
                    }
                    _ => {
                        self.unknown_method(ty, name);
                        self.check_all(cx, args);
                        error_expr(span)
                    }
                }
            }
            TyKind::Dyn(trait_id) | TyKind::SelfType(trait_id) => match self.find_trait_method(trait_id, &name.name) {
                Some((declaring, _, index)) => self.trait_method_call(cx, receiver, declaring, index, args, span),
                None => {
                    self.unknown_method(ty, name);
                    self.check_all(cx, args);
                    error_expr(span)
                }
            },
            TyKind::Param(index) => {
                let found = self.params[index as usize].iter().find_map(|b| self.find_trait_method(*b, &name.name));
                match found {
                    Some((declaring, _, method)) => self.trait_method_call(cx, receiver, declaring, method, args, span),
                    None => {
                        let shown = self.display(ty);
                        self.error(
                            Diagnostic::error(
                                codes::NO_SUCH_MEMBER,
                                format!("`{shown}` has no method `{}`", name.name),
                                name.span,
                                "no such method",
                            )
                            .with_help(format!(
                                "`{shown}` is a type parameter, so only the methods of its bounds are available; add a bound such as `{shown}: Display`"
                            )),
                        );
                        self.check_all(cx, args);
                        error_expr(span)
                    }
                }
            }
            TyKind::Error => {
                self.check_all(cx, args);
                error_expr(span)
            }
            TyKind::Task(_) | TyKind::Channel(_) | TyKind::Mutex(_) | TyKind::Atomic | TyKind::Guard(_) => {
                self.sync_method(cx, receiver, name, args, span)
            }
            TyKind::Str
            | TyKind::FrozenList(_)
            | TyKind::FrozenDict(..)
            | TyKind::List(_)
            | TyKind::Dict(..)
            | TyKind::Option(_)
            | TyKind::Result(..)
            | TyKind::Int
            | TyKind::Float
            | TyKind::Bool
            | TyKind::Tuple(_) => self.builtin_method(cx, receiver, name, args, span),
            _ => {
                let shown = self.display(ty);
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

    fn unknown_method(&mut self, ty: Ty, name: &ast::Ident) {
        let shown = self.display(ty);
        let b = self.builtin();
        let is_field =
            self.struct_of(ty).is_some_and(|id| self.structs[id.0].fields.iter().any(|f| f.name == name.name));
        let derivable = match name.name.as_str() {
            "to_str" => Some("Display"),
            "eq" => Some("Eq"),
            "hash" => Some("Hash"),
            "copy" => Some("Copy"),
            _ => None,
        };
        let mut diagnostic = Diagnostic::error(
            codes::NO_SUCH_MEMBER,
            format!("`{shown}` has no method `{}`", name.name),
            name.span,
            if is_field { "this is a field" } else { "no such method" },
        );
        if is_field {
            diagnostic = diagnostic.with_help(format!("fields cannot be called; read it with `.{}`", name.name));
        } else if let (Some(tr), TyKind::Struct(_) | TyKind::Enum(_)) = (derivable, self.types.kind(ty)) {
            diagnostic = diagnostic.with_help(format!("add `@derive({tr})` to `{shown}`, or implement `{tr}` for it"));
        } else {
            let mut candidates: Vec<String> =
                self.inherent.keys().filter(|(t, _)| *t == ty).map(|(_, m)| m.clone()).collect();
            candidates.extend(self.trait_methods.keys().filter(|(t, _)| *t == ty).map(|(_, m)| m.clone()));
            if let TyKind::Dyn(t) | TyKind::SelfType(t) = *self.types.kind(ty) {
                for tr in self.trait_closure(t) {
                    candidates.extend(self.traits[tr.0].methods.iter().map(|m| m.name.clone()));
                }
            }
            if let Some(s) = super::closest(&name.name, candidates.iter().map(String::as_str)) {
                diagnostic = diagnostic.with_help(format!("did you mean `{s}`?"));
            }
        }
        let _ = b;
        self.error(diagnostic);
    }

    /// `Point(1, 2)` or `Point(x=1, y=2)`. Fields left out take their defaults.
    pub(super) fn construct(
        &mut self,
        cx: &mut FnCx,
        id: crate::types::StructId,
        args: &[ast::Arg],
        span: Span,
    ) -> hir::Expr {
        if !self.check_constructible(id, args, span) {
            self.check_all_quietly(cx, args);
            return error_expr(span);
        }
        let info = &self.structs[id.0];
        let (struct_name, struct_span, struct_ty, fields) =
            (info.name.clone(), info.span, info.ty, info.fields.clone());
        match self.field_values(cx, &struct_name, struct_span, &fields, args, span) {
            Some(values) => mk(K::StructNew { id, fields: values }, struct_ty, span),
            None => error_expr(span),
        }
    }

    /// Values for each field of a struct or variant, from positional and keyword arguments, with
    /// defaults for the rest. `None` after reporting a missing field.
    fn field_values(
        &mut self,
        cx: &mut FnCx,
        owner: &str,
        owner_span: Span,
        fields: &[super::FieldInfo],
        args: &[ast::Arg],
        span: Span,
    ) -> Option<Vec<hir::Expr>> {
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
                                    "`{owner}` has {}, but more values were given",
                                    super::plural(fields.len(), "field")
                                ),
                                arg.value.span,
                                "extra value",
                            )
                            .with_label(owner_span, format!("`{owner}` is defined here")),
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
                            format!("`{owner}` has no field `{}`", name.name),
                            name.span,
                            "no such field",
                        )
                        .with_label(owner_span, format!("`{owner}` is defined here"));
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
                    format!("missing {word} {list} in `{owner}(...)`"),
                    span,
                    format!("missing {list}"),
                )
                .with_label(owner_span, format!("`{owner}` is defined here")),
            );
            return None;
        }
        Some(
            values
                .into_iter()
                .zip(fields)
                .map(|(value, field)| {
                    value.unwrap_or_else(|| hir::Expr { span, ..field.default.clone().expect("checked above") })
                })
                .collect(),
        )
    }

    // ---- fields, indexing, slicing ------------------------------------------------------

    fn attr(&mut self, cx: &mut FnCx, object: &ast::Expr, name: &ast::Ident, expected: Option<Ty>) -> hir::Expr {
        let span = object.span.to(name.span);
        // `Tree[int].Leaf`
        if let ast::ExprKind::Index { object: inner, index } = &object.kind
            && let Some(Global::GenericEnum(template)) = self.generic_name(cx, inner)
        {
            let Some(ty) = self.explicit_instance(template, index, object.span) else { return error_expr(span) };
            let id = self.enum_of(ty).expect("an enum instance");
            if let Some(variant) = self.enums[id.0].variants.iter().position(|v| v.name == name.name) {
                return self.variant_value(cx, id, variant, None, span);
            }
            let shown = self.display(ty);
            self.error(Diagnostic::error(
                codes::NO_SUCH_MEMBER,
                format!("`{shown}` has no variant `{}`", name.name),
                name.span,
                "no such variant",
            ));
            return error_expr(span);
        }
        // `module.name`
        if let Some(global) = self.member_global(cx, object, name) {
            let shown = format!("{}.{}", Self::dotted(object).unwrap_or_default(), name.name);
            return self.global_value(cx, global, &shown, span, expected);
        }
        if let Some(type_name) = Self::dotted(object)
            && !cx.sees(type_name.split('.').next().unwrap_or_default())
        {
            let type_name = type_name.as_str();
            let global = self.path_global(cx, object);
            if let Some(Global::GenericEnum(template)) = global {
                let pattern = self.pattern_of(template);
                if let Some(id) = self.enum_of(pattern)
                    && let Some(variant) = self.enums[id.0].variants.iter().position(|v| v.name == name.name)
                {
                    return self.generic_variant(cx, template, variant, None, None, span, expected);
                }
            }
            match global {
                Some(Global::Enum(id)) => {
                    if let Some(index) = self.enums[id.0].variants.iter().position(|v| v.name == name.name) {
                        return self.variant_value(cx, id, index, None, span);
                    }
                    let candidates: Vec<String> = self.enums[id.0].variants.iter().map(|v| v.name.clone()).collect();
                    let enum_span = self.enums[id.0].span;
                    let mut diagnostic = Diagnostic::error(
                        codes::NO_SUCH_MEMBER,
                        format!("`{type_name}` has no variant `{}`", name.name),
                        name.span,
                        "no such variant",
                    )
                    .with_label(enum_span, format!("`{type_name}` is defined here"));
                    if let Some(s) = super::closest(&name.name, candidates.iter().map(String::as_str)) {
                        diagnostic = diagnostic.with_help(format!("did you mean `{s}`?"));
                    }
                    self.error(diagnostic);
                    return error_expr(span);
                }
                Some(Global::Struct(id)) if self.inherent.contains_key(&(self.structs[id.0].ty, name.name.clone())) => {
                    let func = self.inherent[&(self.structs[id.0].ty, name.name.clone())];
                    return self.method_ref(func, span);
                }
                Some(Global::Struct(_)) => {
                    self.error(
                        Diagnostic::error(codes::NOT_SUPPORTED, format!("`{type_name}.{}` needs a value, not the type", name.name), span, "used on the type")
                            .with_help(format!("read fields from a value of type `{type_name}`; call static methods as `{type_name}.{}(...)`", name.name)),
                    );
                    return error_expr(span);
                }
                _ => {}
            }
        }
        let object = self.expr(cx, object, None);
        // `value.method` without a call: a bound method.
        let is_field =
            self.struct_of(object.ty).is_some_and(|id| self.structs[id.0].fields.iter().any(|f| f.name == name.name));
        if !is_field
            && matches!(self.types.kind(object.ty), TyKind::Struct(_) | TyKind::Enum(_))
            && let Some(func) = self.inherent.get(&(object.ty, name.name.clone())).copied().or_else(|| {
                match self.trait_methods.get(&(object.ty, name.name.clone())).map(Vec::as_slice) {
                    Some([(trait_id, index)]) => {
                        self.impl_of(*trait_id, object.ty).and_then(|i| self.impls[i].methods[*index])
                    }
                    _ => None,
                }
            })
            && self.signatures[func.0].receiver.is_some()
        {
            return self.bound_method(object, func, span);
        }
        if object.ty != Ty::ERROR && self.struct_of(object.ty).is_none() {
            let shown = self.display(object.ty);
            let is_method = self.is_builtin_method(object.ty, &name.name)
                || self.inherent.contains_key(&(object.ty, name.name.clone()))
                || self.trait_methods.contains_key(&(object.ty, name.name.clone()));
            if is_method {
                self.error(
                    Diagnostic::error(
                        codes::NO_SUCH_MEMBER,
                        format!("`{shown}` has no field `{}`", name.name),
                        name.span,
                        "this is a method",
                    )
                    .with_help(format!("call it: `.{}()`", name.name)),
                );
                return error_expr(span);
            }
            self.field_of(&object, name);
            return error_expr(span);
        }
        match self.field_of(&object, name) {
            Some((index, ty, _)) => mk(K::Field { object: Box::new(object), index }, ty, span),
            None => error_expr(span),
        }
    }

    fn index(&mut self, cx: &mut FnCx, object: &ast::Expr, index: &ast::Expr, span: Span) -> hir::Expr {
        let container = self.expr(cx, object, None);
        let container = if matches!(self.types.kind(container.ty), TyKind::Struct(_) | TyKind::Enum(_)) {
            match self.index_protocol(cx, container, index, span) {
                Ok(value) => return value,
                Err(container) => container,
            }
        } else {
            container
        };
        match self.types.kind(container.ty).clone() {
            TyKind::List(elem) | TyKind::FrozenList(elem) => {
                let index = self.expect(cx, index, Ty::INT, None);
                mk(K::Index { container: Box::new(container), index: Box::new(index) }, elem, span)
            }
            TyKind::Dict(key, value) | TyKind::FrozenDict(key, value) => {
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
            TyKind::List(_) | TyKind::Str | TyKind::FrozenList(_) => {
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
                if !self.hashable(key.ty) {
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
                    if !self.displayable(value.ty) {
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
        let help = match self.types.kind(ty) {
            TyKind::Struct(_) | TyKind::Enum(_) => {
                format!("add `@derive(Display)` to `{shown}`, or implement `Display` for it")
            }
            _ => "only values whose types implement `Display` can be printed".to_string(),
        };
        self.error(
            Diagnostic::error(
                codes::NOT_SUPPORTED,
                format!("a `{shown}` cannot be printed"),
                span,
                format!("has type `{shown}`"),
            )
            .with_help(help),
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
        let value = self.expr(cx, operand, expected.filter(|t| *t == Ty::FLOAT || self.types.sized_kind(*t).is_some()));
        if value.ty == Ty::ERROR {
            return error_expr(span);
        }
        if let Some(kind) = self.types.sized_kind(value.ty) {
            let ty = value.ty;
            return match (op, &value.kind) {
                (ast::UnaryOp::Neg, K::Int(v)) => mk(K::Int(v.wrapping_neg()), ty, span),
                (ast::UnaryOp::Neg, K::Float(v)) => mk(K::Float(-v), ty, span),
                (ast::UnaryOp::Neg, _) => {
                    let op = if kind.is_float() { hir::UnaryOp::FloatNeg } else { hir::UnaryOp::IntNeg };
                    mk(K::Unary { op, operand: Box::new(value) }, ty, span)
                }
                (ast::UnaryOp::Plus, _) => value,
                (ast::UnaryOp::Invert, _) if !kind.is_float() => {
                    mk(K::Unary { op: hir::UnaryOp::IntInvert, operand: Box::new(value) }, ty, span)
                }
                _ => {
                    self.error(Diagnostic::error(
                        codes::BAD_OPERANDS,
                        format!("cannot apply unary `{}` to `{}`", op.symbol(), kind.name()),
                        span,
                        format!("operand has type `{}`", kind.name()),
                    ));
                    error_expr(span)
                }
            };
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
                // `x is not None and x > 3`: the right side runs only when the left holds.
                let narrowed = self.narrowings(cx, lhs, op == A::And);
                let before = cx.narrowed.len();
                cx.narrowed.extend(narrowed);
                let r = self.bool_operand(cx, op, rhs);
                cx.narrowed.truncate(before);
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
            let allowed = if ordering { self.orderable(l.ty) } else { self.equatable(l.ty) };
            if !allowed {
                let shown = self.display(l.ty);
                let help = match self.types.kind(l.ty) {
                    TyKind::Struct(_) | TyKind::Enum(_) if ordering => {
                        format!("add `@derive(Ord)` to `{shown}`, or implement `Ord` for it")
                    }
                    TyKind::Param(_) if ordering => format!("add the bound `{shown}: Ord` to the type parameter"),
                    TyKind::Param(_) => format!("add the bound `{shown}: Eq` to the type parameter"),
                    TyKind::Struct(_) => format!(
                        "add `@derive(Eq)` to `{shown}` to compare values; use `is` to check whether two values are the same object"
                    ),
                    TyKind::Enum(_) => format!("add `@derive(Eq)` to `{shown}`, or use `match`"),
                    _ if ordering => "`<` and friends work on numbers, strings and booleans".to_string(),
                    _ => "values of this type cannot be compared with `==`".to_string(),
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

        // `+`, `-` and `*` on a type parameter use its number-trait bounds; the instances use
        // the real operator.
        if l.ty == r.ty && matches!(self.types.kind(l.ty), TyKind::Param(_)) && matches!(op, A::Add | A::Sub | A::Mul) {
            let b = self.builtin();
            let trait_id = match op {
                A::Add => b.add,
                A::Sub => b.sub,
                _ => b.mul,
            };
            if self.implements(l.ty, trait_id) {
                let ty = l.ty;
                return mk(K::Binary { op: B::IntAdd, lhs: Box::new(l), rhs: Box::new(r) }, ty, span);
            }
            return self.bad_operands(op, &l, &r, span);
        }
        // `+`, `-` and `*` on user types call the number traits.
        if l.ty == r.ty
            && matches!(self.types.kind(l.ty), TyKind::Struct(_) | TyKind::Enum(_))
            && matches!(op, A::Add | A::Sub | A::Mul)
        {
            let b = self.builtin();
            let trait_id = match op {
                A::Add => b.add,
                A::Sub => b.sub,
                _ => b.mul,
            };
            if let Some(index) = self.impl_of(trait_id, l.ty)
                && let Some(func) = self.impls[index].methods[0]
            {
                let ret = self.signatures[func.0].ret;
                return mk(K::Call { func, args: vec![l, r] }, ret, span);
            }
            return self.bad_operands(op, &l, &r, span);
        }

        // The sized number types have the basic operators; anything else converts to `int` or
        // `float` first.
        if let (Some(kind), true) = (self.types.sized_kind(l.ty), l.ty == r.ty) {
            let hir_op = match (op, kind.is_float()) {
                (A::Add, false) => Some(B::IntAdd),
                (A::Sub, false) => Some(B::IntSub),
                (A::Mul, false) => Some(B::IntMul),
                (A::BitAnd, false) => Some(B::IntAnd),
                (A::BitOr, false) => Some(B::IntOr),
                (A::BitXor, false) => Some(B::IntXor),
                (A::Add, true) => Some(B::FloatAdd),
                (A::Sub, true) => Some(B::FloatSub),
                (A::Mul, true) => Some(B::FloatMul),
                (A::Div, true) => Some(B::FloatDiv),
                _ => None,
            };
            if let Some(hir_op) = hir_op {
                let ty = l.ty;
                return mk(K::Binary { op: hir_op, lhs: Box::new(l), rhs: Box::new(r) }, ty, span);
            }
            let (symbol, name) = (op.symbol(), kind.name());
            let to = if kind.is_float() { "float" } else { "int" };
            self.error(
                Diagnostic::error(
                    codes::BAD_OPERANDS,
                    format!("`{symbol}` is not available on `{name}`"),
                    span,
                    format!("both sides are `{name}`"),
                )
                .with_help(format!("convert first: `{to}(a) {symbol} {to}(b)`")),
            );
            return error_expr(span);
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
            (A::Mul, TyKind::Str, TyKind::Int) => (B::StrRepeat, Ty::STR),
            (A::Add, TyKind::List(_), TyKind::List(_)) if l.ty == r.ty => (B::ListConcat, l.ty),
            _ => return self.bad_operands(op, &l, &r, span),
        };
        mk(K::Binary { op: hir_op, lhs: Box::new(l), rhs: Box::new(r) }, ty, span)
    }

    fn bad_operands(&mut self, op: ast::BinaryOp, l: &hir::Expr, r: &hir::Expr, span: Span) -> hir::Expr {
        let (ls, rs) = (self.display(l.ty), self.display(r.ty));
        let symbol = op.symbol();
        let help = match (self.types.kind(l.ty), self.types.kind(r.ty)) {
            (TyKind::Int, TyKind::Float) | (TyKind::Float, TyKind::Int) => {
                "numbers are not converted implicitly; convert one side with `float(...)` or `int(...)`".to_string()
            }
            (TyKind::Str, _) | (_, TyKind::Str) => {
                "strings only combine with strings; use an f-string or `str(...)` to build text".to_string()
            }
            (TyKind::Struct(_) | TyKind::Enum(_) | TyKind::Param(_), _)
                if l.ty == r.ty && matches!(op, ast::BinaryOp::Add | ast::BinaryOp::Sub | ast::BinaryOp::Mul) =>
            {
                let tr = match op {
                    ast::BinaryOp::Add => "AddNumber",
                    ast::BinaryOp::Sub => "SubNumber",
                    _ => "MulNumber",
                };
                format!("implement `{tr}` for `{ls}` to use `{symbol}` on it")
            }
            _ => "both sides must have types that support this operator".to_string(),
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
            TyKind::List(elem) | TyKind::FrozenList(elem) => {
                if !self.equatable(elem) {
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
            TyKind::Dict(key, _) | TyKind::FrozenDict(key, _) => self.expect(cx, needle, key, None),
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
