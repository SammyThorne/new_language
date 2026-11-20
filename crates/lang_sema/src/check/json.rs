//! Type checking for JSON (docs/05-standard-library.md, "JSON").
//!
//! The `json` module is ordinary code in the standard library (`stdlib/json.lang`), with two
//! intrinsics the compiler expands: `__json_encode(value)` and `__json_decode(text)`, whose
//! target type comes from the expected `Result[T, JsonError]`. Code generation writes an encoder
//! and a decoder for each type that is encoded or decoded.
//!
//! Which types are JSON-capable: numbers, `bool`, `str`, `None`, optionals, lists, tuples,
//! `dict[str, V]`, the immutable collections of those, `json.Value`, and structs and enums with
//! `@derive(Json)`.
//!
//! A literal written where a `json.Value` is expected becomes one: `True`, `3`, `"x"`, `None`,
//! and list and dict literals whose elements become values in turn.

use super::{Checker, FnCx, Global, error_expr};
use crate::hir::{self, Builtin as Op, ExprKind as K};
use crate::types::{Ty, TyKind};
use lang_diag::{Diagnostic, Span, codes};
use lang_syntax::ast;

fn mk(kind: hir::ExprKind, ty: Ty, span: Span) -> hir::Expr {
    hir::Expr { kind, ty, span }
}

impl Checker<'_, '_> {
    /// Find `json.Value` and `json.JsonError` if the program imports `json`.
    pub(super) fn find_json_types(&mut self) {
        let Some(module) = self.module_names.iter().position(|n| n == "json") else { return };
        let value = match self.exports[module].get("Value") {
            Some((Global::Enum(id), _, _)) => *id,
            _ => return,
        };
        let error = match self.exports[module].get("JsonError") {
            Some((Global::Struct(id), _, _)) => *id,
            _ => return,
        };
        self.json = Some(hir::JsonTypes { value, error });
    }

    pub(super) fn json_value_ty(&self) -> Option<Ty> {
        self.json.map(|j| self.enums[j.value.0].ty)
    }

    /// Whether `json.encode` and `json.decode` work on a type.
    pub(super) fn json_capable(&self, ty: Ty) -> bool {
        self.json_capable_for(ty, self.builtin().to_json)
    }

    /// Whether a type has `ToJson` (or `FromJson`, as `trait_id` says), built in or derived.
    pub(super) fn json_capable_for(&self, ty: Ty, trait_id: crate::types::TraitId) -> bool {
        match self.types.kind(ty) {
            TyKind::Error | TyKind::Int | TyKind::Float | TyKind::Bool | TyKind::Str | TyKind::None => true,
            TyKind::Sized(_) => true,
            TyKind::Option(t) | TyKind::List(t) | TyKind::FrozenList(t) => self.json_capable_for(*t, trait_id),
            // A set is an array of its elements.
            TyKind::Dict(k, Ty::NONE) => self.json_capable_for(*k, trait_id),
            TyKind::Dict(k, v) | TyKind::FrozenDict(k, v) => *k == Ty::STR && self.json_capable_for(*v, trait_id),
            TyKind::Tuple(items) => items.iter().all(|t| self.json_capable_for(*t, trait_id)),
            TyKind::Struct(_) | TyKind::Enum(_) | TyKind::Param(_) | TyKind::Dyn(_) | TyKind::SelfType(_) => {
                self.implements(ty, trait_id)
            }
            _ => false,
        }
    }

    fn not_json(&mut self, ty: Ty, span: Span, what: &str) {
        let shown = self.display(ty);
        let help = match self.types.kind(ty) {
            TyKind::Struct(_) | TyKind::Enum(_) => format!("add `@derive(Json)` to `{shown}`"),
            TyKind::Dict(..) | TyKind::FrozenDict(..) => "JSON objects have string keys; use a `dict[str, V]`".to_string(),
            _ => "JSON works with numbers, strings, booleans, lists, dicts with `str` keys, and types with `@derive(Json)`"
                .to_string(),
        };
        self.error(
            Diagnostic::error(
                codes::NOT_SUPPORTED,
                format!("a `{shown}` cannot be {what}"),
                span,
                format!("has type `{shown}`"),
            )
            .with_help(help),
        );
    }

    /// `__json_encode(value)` and `__json_decode(text)`, used by the `json` module.
    pub(super) fn json_intrinsic(
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
                format!("`{name}` takes one argument"),
                span,
                "expected one",
            ));
            self.check_all_quietly(cx, args);
            return error_expr(span);
        };
        if name == "__json_encode" {
            let value = self.expr(cx, &arg.value, None);
            if !self.types.contains_error(value.ty) && !self.json_capable(value.ty) {
                self.not_json(value.ty, value.span, "written as JSON");
                return error_expr(span);
            }
            return mk(K::Builtin { op: Op::JsonEncode, args: vec![value] }, Ty::STR, span);
        }
        let text = self.expect(cx, &arg.value, Ty::STR, None);
        let Some(result) = expected.filter(|t| self.types.result_parts(*t).is_some()) else {
            self.error(Diagnostic::error(
                codes::CANNOT_INFER,
                "cannot tell what type to decode",
                span,
                "use it where a `Result[T, JsonError]` is expected",
            ));
            return error_expr(span);
        };
        let (target, _) = self.types.result_parts(result).expect("checked");
        if self.types.contains_error(target) {
            return error_expr(span);
        }
        if !self.json_capable_for(target, self.builtin().from_json) {
            self.not_json(target, span, "read from JSON");
            return error_expr(span);
        }
        mk(K::Builtin { op: Op::JsonDecode, args: vec![text] }, result, span)
    }

    /// Whether this module may use the JSON intrinsics: only the standard library's `json`.
    pub(super) fn in_json_module(&self) -> bool {
        self.module_names.get(self.current_module).is_some_and(|n| n == "json")
    }

    /// A literal where a `json.Value` is expected: a list or dict literal becomes an `Array` or
    /// `Object` whose elements are values in turn. `None` if the expression is not one.
    pub(super) fn json_literal(&mut self, cx: &mut FnCx, expr: &ast::Expr, expected: Option<Ty>) -> Option<hir::Expr> {
        let value_ty = self.json_value_ty()?;
        if expected != Some(value_ty) {
            return None;
        }
        let span = expr.span;
        match &expr.kind {
            ast::ExprKind::List(_) => {
                let list_ty = self.types.list(value_ty);
                let list = self.expr(cx, expr, Some(list_ty));
                let list = self.coerce(list, list_ty, None);
                Some(self.json_variant("Array", list, span))
            }
            ast::ExprKind::Dict(_) => {
                let dict_ty = self.types.dict(Ty::STR, value_ty);
                let dict = self.expr(cx, expr, Some(dict_ty));
                let dict = self.coerce(dict, dict_ty, None);
                Some(self.json_variant("Object", dict, span))
            }
            _ => None,
        }
    }

    fn json_variant(&mut self, variant: &str, payload: hir::Expr, span: Span) -> hir::Expr {
        let Some(json) = self.json else { return error_expr(span) };
        let info = &self.enums[json.value.0];
        let ty = info.ty;
        let index = info.variants.iter().position(|v| v.name == variant).expect("json.Value has this variant");
        let fields = if variant == "Null" { Vec::new() } else { vec![payload] };
        mk(K::Variant { index, fields }, ty, span)
    }

    /// The implicit conversion of a scalar to a `json.Value`; `None` if it does not apply.
    pub(super) fn json_value_of(&mut self, value: hir::Expr, expected: Ty) -> Result<hir::Expr, hir::Expr> {
        if self.json_value_ty() != Some(expected) {
            return Err(value);
        }
        let span = value.span;
        let variant = match value.ty {
            Ty::BOOL => "Bool",
            Ty::INT => "Int",
            Ty::FLOAT => "Float",
            Ty::STR => "Str",
            Ty::NONE if matches!(value.kind, K::None) => "Null",
            _ => return Err(value),
        };
        Ok(self.json_variant(variant, value, span))
    }

    /// `x[key]` on a struct or enum: calls `get_index(self, i: int)` for an `int` key and
    /// `get_item(self, key)` otherwise. `None` if the type has neither.
    pub(super) fn index_protocol(
        &mut self,
        cx: &mut FnCx,
        container: hir::Expr,
        index: &ast::Expr,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let ty = container.ty;
        let by_index = self.one_param_method(ty, "get_index");
        let by_key = self.one_param_method(ty, "get_item");
        if by_index.is_none() && by_key.is_none() {
            return Err(container);
        }
        let key = self.expr(cx, index, None);
        let func = match (by_index, by_key) {
            (Some(f), _) if key.ty == Ty::INT => f,
            (_, Some(f)) => f,
            (Some(f), None) => f,
            (None, None) => unreachable!(),
        };
        let param = self.signatures[func.0].params[0].ty;
        let key = self.coerce(key, param, None);
        let ret = self.signatures[func.0].ret;
        Ok(mk(K::Call { func, args: vec![container, key] }, ret, span))
    }

    fn one_param_method(&self, ty: Ty, name: &str) -> Option<hir::FuncId> {
        let func = *self.inherent.get(&(ty, name.to_string()))?;
        let signature = &self.signatures[func.0];
        (signature.receiver.is_some() && signature.params.len() == 1).then_some(func)
    }
}
