//! Function values: lambdas, nested functions and the variables they capture, named functions
//! and bound methods used as values, and calls through function values. Also comprehensions,
//! which run a loop inside an expression, and flow narrowing of optional variables.
//!
//! A lambda or nested function is checked with its own [`FnCx`], whose `outer` field holds the
//! enclosing function's context while it is checked. Reading a name the closure does not define
//! looks outwards; a variable found there is *captured*: the enclosing function keeps it in a
//! heap cell (`boxed`), and the closure reaches the cell through its environment. Captures are
//! by reference, as in Python, so a closure sees later assignments. A closure cannot assign to
//! a captured variable (E0332); a list or struct field can carry changing state instead.

use super::{BodyMode, Checker, FnCx, LocalInfo, ParamSig, Signature, error_expr, plural, prescan};
use crate::hir::{self, ExprKind as K, FuncId, LocalId};
use crate::types::{Ty, TyKind};
use lang_diag::{Diagnostic, Span, codes};
use lang_syntax::ast;
use std::collections::HashMap;

fn mk(kind: hir::ExprKind, ty: Ty, span: Span) -> hir::Expr {
    hir::Expr { kind, ty, span }
}

/// The body of a closure: a lambda's expression or a nested function's statements.
enum ClosureBody<'e> {
    Expr(&'e ast::Expr),
    Stmts(&'e [ast::Stmt]),
}

/// What a comprehension builds: `[x for ...]`, `{x for ...}`, or `{k: v for ...}` with the
/// value expression.
#[derive(Clone, Copy)]
pub(super) enum CompKind<'a> {
    List,
    Set,
    Dict(&'a ast::Expr),
}

impl Checker<'_, '_> {
    /// The local a name refers to, capturing it from an enclosing function if needed.
    pub(super) fn lookup_local(&mut self, cx: &mut FnCx, name: &str) -> Option<LocalId> {
        if let Some(&id) = cx.scope.get(name) {
            return Some(id);
        }
        let outer = cx.outer.as_mut()?;
        let outer_id = self.lookup_local(outer, name)?;
        outer.info[outer_id.0].read = true;
        outer.locals[outer_id.0].boxed = true;
        let (ty, decl_span, assigned) =
            (outer.locals[outer_id.0].ty, outer.info[outer_id.0].decl_span, outer.is_assigned(outer_id));
        let slot = cx.captures.len();
        cx.captures.push(outer_id);
        let id = LocalId(cx.locals.len());
        cx.locals.push(hir::Local { name: name.to_string(), ty, boxed: true, capture: Some(slot) });
        // Counted as a parameter so it is never reported as unused here.
        cx.info.push(LocalInfo { decl_span, read: false, is_param: true, hidden: false });
        cx.assigned.push(false);
        if assigned {
            cx.set_assigned(id);
        }
        cx.scope.insert(name.to_string(), id);
        Some(id)
    }

    /// `lambda x, y: body`. The parameter types come from the function type expected here.
    pub(super) fn lambda(
        &mut self,
        cx: &mut FnCx,
        params: &[ast::Ident],
        body: &ast::Expr,
        expected: Option<Ty>,
        span: Span,
    ) -> hir::Expr {
        // Where an optional function is expected (`allow: (def(str) -> bool)? = None`), the
        // lambda is the function, and the conversion to the optional happens as for any value.
        let expected = expected.map(|t| match self.types.kind(t) {
            &TyKind::Option(inner) if matches!(self.types.kind(inner), TyKind::Func(..)) => inner,
            _ => t,
        });
        let expected_func = expected.and_then(|t| match self.types.kind(t).clone() {
            TyKind::Func(ps, r) => Some((ps, r)),
            _ => None,
        });
        let Some((param_tys, ret)) = expected_func else {
            if expected.is_some_and(|t| self.types.contains_error(t)) {
                return error_expr(span);
            }
            if params.is_empty() {
                // Nothing to infer: the type comes from the body.
                return self.lambda_with(cx, params, body, Vec::new(), None, span);
            }
            self.error(
                Diagnostic::error(
                    codes::CANNOT_INFER,
                    "cannot infer the types of this lambda's parameters",
                    span,
                    "parameter types unknown",
                )
                .with_help("use a lambda where a function type is expected, such as an argument, or define the function with `def` and type hints"),
            );
            return error_expr(span);
        };
        if param_tys.len() != params.len() {
            let shown = self.display(expected.unwrap_or(Ty::ERROR));
            self.error(Diagnostic::error(
                codes::TYPE_MISMATCH,
                format!(
                    "this lambda takes {}, but `{shown}` takes {}",
                    plural(params.len(), "parameter"),
                    param_tys.len()
                ),
                span,
                format!("expected `{shown}`"),
            ));
            return error_expr(span);
        }
        self.lambda_with(cx, params, body, param_tys, Some(ret), span)
    }

    /// A lambda with known parameter types. Its return type is `ret`, or the type of its body
    /// when `ret` is `None`.
    pub(super) fn lambda_with(
        &mut self,
        cx: &mut FnCx,
        params: &[ast::Ident],
        body: &ast::Expr,
        param_tys: Vec<Ty>,
        ret: Option<Ty>,
        span: Span,
    ) -> hir::Expr {
        let params: Vec<ParamSig> = params
            .iter()
            .zip(param_tys)
            .map(|(p, ty)| ParamSig { name: p.name.clone(), ty, hint_span: p.span, default: None })
            .collect();
        let name = format!("{}.<lambda>", cx.name);
        let (func, captures, ty) = self.closure(cx, name, params, ret, None, ClosureBody::Expr(body), span);
        mk(K::Closure { func, captures }, ty, span)
    }

    /// `def name(...):` inside a function: a closure assigned to a local of that name, which
    /// is already assigned inside its own body so the function can call itself.
    pub(super) fn nested_def(&mut self, cx: &mut FnCx, function: &ast::Function) -> Vec<hir::Stmt> {
        let span = function.name.span;
        if !function.generics.is_empty() {
            self.error(
                Diagnostic::error(
                    codes::NOT_SUPPORTED,
                    "nested functions cannot have type parameters",
                    span,
                    "not supported",
                )
                .with_help("define the generic function at the top level of the file"),
            );
            return Vec::new();
        }
        if let Some(self_span) = function.self_param {
            self.error(Diagnostic::error(
                codes::UNEXPECTED_TOKEN,
                "`self` is only allowed in methods",
                self_span,
                "not inside an `impl` block",
            ));
            return Vec::new();
        }
        let signature = self.signature(function, &function.name.name, None);
        if let Some(param) = function.params.iter().find(|p| p.default.is_some()) {
            self.error(Diagnostic::error(
                codes::NOT_SUPPORTED,
                "parameters of nested functions cannot have default values",
                param.name.span,
                "not supported for function values",
            ));
        }
        let func_ty = self.types.func(signature.params.iter().map(|p| p.ty).collect(), signature.ret);
        let name = &function.name.name;
        if !cx.scope.contains_key(name)
            && let Some(kind) = self.local_conflict(name)
        {
            self.error(Diagnostic::error(
                codes::DUPLICATE_DEFINITION,
                format!("cannot define a nested function named `{name}`; it is {kind}"),
                span,
                "already defined",
            ));
            return Vec::new();
        }
        let local = match cx.scope.get(name) {
            Some(&local) => {
                let declared = cx.locals[local.0].ty;
                if !self.types.fits(func_ty, declared) {
                    let (a, b) = (self.display(declared), self.display(func_ty));
                    self.error(Diagnostic::error(
                        codes::TYPE_MISMATCH,
                        format!("`{name}` already has type `{a}`, but this function is `{b}`"),
                        span,
                        "redefined with another type",
                    ));
                }
                local
            }
            None => cx.add_local(name, func_ty, span, false),
        };
        cx.info[local.0].read = true;
        cx.set_assigned(local);
        let qualified = format!("{}.{name}", cx.name);
        let (func, captures, ty) = self.closure(
            cx,
            qualified,
            signature.params,
            Some(signature.ret),
            signature.ret_span,
            ClosureBody::Stmts(&function.body),
            span,
        );
        let value = mk(K::Closure { func, captures }, ty, span);
        vec![hir::Stmt::Assign { place: hir::Place::Local(local), value }]
    }

    #[allow(clippy::too_many_arguments)]
    fn closure(
        &mut self,
        cx: &mut FnCx,
        name: String,
        params: Vec<ParamSig>,
        ret: Option<Ty>,
        ret_span: Option<Span>,
        body: ClosureBody,
        span: Span,
    ) -> (FuncId, Vec<LocalId>, Ty) {
        let func = FuncId(self.signatures.len());
        self.signatures.push(Signature {
            name: name.clone(),
            name_span: span,
            params: params.clone(),
            ret: ret.unwrap_or(Ty::ERROR),
            ret_span,
            receiver: None,
            private: false,
        });
        self.bodies.push(None);

        let outer = std::mem::replace(cx, FnCx::detached());
        let mut inner = FnCx::new(&name, ret.unwrap_or(Ty::ERROR), ret_span);
        inner.outer = Some(Box::new(outer));
        for param in &params {
            let local = inner.add_local(&param.name, param.ty, param.hint_span, true);
            inner.set_assigned(local);
        }
        let param_count = params.len();
        let (body, ret) = match body {
            ClosureBody::Expr(expr) => {
                let value = match ret {
                    Some(Ty::NONE) | None => self.expr(&mut inner, expr, None),
                    Some(ret) => self.expect(&mut inner, expr, ret, None),
                };
                let ret = ret.unwrap_or(value.ty);
                if ret == Ty::NONE {
                    (vec![hir::Stmt::Expr(value), hir::Stmt::Return(None)], ret)
                } else {
                    (vec![hir::Stmt::Return(Some(value))], ret)
                }
            }
            ClosureBody::Stmts(stmts) => {
                let ret = ret.unwrap_or(Ty::NONE);
                inner.ret = ret;
                prescan(stmts, &mut inner.first_assignment);
                let (mut body, diverges) = self.block(&mut inner, stmts);
                self.finish_body(&inner, &mut body, diverges, span);
                (body, ret)
            }
        };
        self.warn_unused(&inner);
        *cx = *inner.outer.take().expect("the enclosing context is restored");

        let param_tys: Vec<Ty> = params.iter().map(|p| p.ty).collect();
        let generic = matches!(self.mode, BodyMode::GenericCheck | BodyMode::Skip)
            || self.diags.speculative
            || inner.locals.iter().any(|l| self.types.contains_param(l.ty));
        self.signatures[func.0].ret = ret;
        self.closures.push((
            func,
            hir::Function {
                name,
                params: (0..param_count).map(LocalId).collect(),
                locals: inner.locals,
                ret,
                body,
                generic,
                closure: true,
                extern_symbol: None,
                span: Some(span),
            },
        ));
        let ty = self.types.func(param_tys, ret);
        (func, inner.captures, ty)
    }

    /// A named function or static method used as a value.
    pub(super) fn func_ref(&mut self, func: FuncId, span: Span) -> hir::Expr {
        let signature = &self.signatures[func.0];
        let (params, ret) = (signature.params.iter().map(|p| p.ty).collect(), signature.ret);
        let ty = self.types.func(params, ret);
        mk(K::FuncRef(func), ty, span)
    }

    /// `Type.method` as a value: a function whose first parameter is the receiver, if any.
    pub(super) fn method_ref(&mut self, func: FuncId, span: Span) -> hir::Expr {
        let signature = &self.signatures[func.0];
        let mut params: Vec<Ty> = signature.receiver.iter().map(|(t, _)| *t).collect();
        params.extend(signature.params.iter().map(|p| p.ty));
        let ty = self.types.func(params, signature.ret);
        mk(K::FuncRef(func), ty, span)
    }

    /// A built-in function used as a value, such as `key=len`: a lambda that calls it, for the
    /// parameter types the context expects.
    pub(super) fn builtin_value(&mut self, cx: &mut FnCx, name: &str, params: Vec<Ty>, span: Span) -> hir::Expr {
        let idents: Vec<ast::Ident> =
            (0..params.len()).map(|i| ast::Ident { name: format!("__arg{i}"), span }).collect();
        let args: Vec<ast::Arg> = idents
            .iter()
            .map(|id| ast::Arg { name: None, value: ast::Expr { kind: ast::ExprKind::Name(id.name.clone()), span } })
            .collect();
        let body = ast::Expr {
            kind: ast::ExprKind::Call {
                callee: Box::new(ast::Expr { kind: ast::ExprKind::Name(name.to_string()), span }),
                args,
            },
            span,
        };
        self.lambda_with(cx, &idents, &body, params, None, span)
    }

    /// `value.method` without a call: a function value that calls the method on `value`.
    pub(super) fn bound_method(&mut self, receiver: hir::Expr, func: FuncId, span: Span) -> hir::Expr {
        let signature = &self.signatures[func.0];
        let (params, ret) = (signature.params.iter().map(|p| p.ty).collect(), signature.ret);
        let ty = self.types.func(params, ret);
        mk(K::BoundMethod { receiver: Box::new(receiver), func }, ty, span)
    }

    /// A call to a function value.
    pub(super) fn call_value(&mut self, cx: &mut FnCx, callee: hir::Expr, args: &[ast::Arg], span: Span) -> hir::Expr {
        let TyKind::Func(params, ret) = self.types.kind(callee.ty).clone() else {
            if callee.ty != Ty::ERROR {
                let shown = self.display(callee.ty);
                self.error(Diagnostic::error(
                    codes::NOT_A_FUNCTION,
                    format!("a `{shown}` cannot be called"),
                    callee.span,
                    format!("has type `{shown}`"),
                ));
            }
            self.check_all_quietly(cx, args);
            return error_expr(span);
        };
        if let Some(arg) = args.iter().find(|a| a.name.is_some()) {
            self.error(
                Diagnostic::error(
                    codes::BAD_FUNCTION_VALUE,
                    "a function value cannot take keyword arguments",
                    arg.name.as_ref().map_or(span, |n| n.span),
                    "keyword argument",
                )
                .with_help("function types only know the types of their parameters, not their names"),
            );
            self.check_all_quietly(cx, args);
            return error_expr(span);
        }
        if args.len() != params.len() {
            let shown = self.display(callee.ty);
            self.error(Diagnostic::error(
                codes::WRONG_ARG_COUNT,
                format!("this function takes {} but {} given", plural(params.len(), "argument"), args.len()),
                span,
                format!("the function is `{shown}`"),
            ));
            self.check_all_quietly(cx, args);
            return error_expr(span);
        }
        let values = args.iter().zip(params).map(|(a, p)| self.expect(cx, &a.value, p, None)).collect();
        mk(K::CallValue { callee: Box::new(callee), args: values }, ret, span)
    }

    // ---- bodies -------------------------------------------------------------------------

    /// After a body: add the implicit `Ok(None)` for `Result[None]` functions, or report a
    /// missing `return`.
    pub(super) fn finish_body(&mut self, cx: &FnCx, body: &mut hir::Block, diverges: bool, name_span: Span) {
        let (ret, ret_span) = (cx.ret, cx.ret_span);
        if !diverges && self.returns_ok_none(ret) {
            body.push(hir::Stmt::Return(Some(self.ok_none(ret, name_span))));
        } else if !diverges && !matches!(ret, Ty::NONE | Ty::ERROR) {
            let shown = self.display(ret);
            let mut diagnostic = Diagnostic::error(
                codes::MISSING_RETURN,
                format!("function `{}` can finish without returning a value", cx.name),
                name_span,
                "can reach its end without `return`",
            )
            .with_help("add a `return` statement at the end of the function");
            if let Some(span) = ret_span {
                diagnostic = diagnostic.with_label(span, format!("declared to return `{shown}`"));
            }
            self.error(diagnostic);
        }
    }

    pub(super) fn warn_unused(&mut self, cx: &FnCx) {
        for (local, info) in cx.locals.iter().zip(&cx.info) {
            // A variable whose value had an error was already reported.
            let had_error = self.types.contains_error(local.ty);
            if !info.read && !info.is_param && !info.hidden && !local.name.starts_with('_') && !had_error {
                self.diags.push(
                    Diagnostic::warning(
                        codes::UNUSED_VARIABLE,
                        format!("variable `{}` is assigned but never used", local.name),
                        info.decl_span,
                        "assigned here",
                    )
                    .with_help(format!("if this is intentional, start the name with an underscore: `_{}`", local.name)),
                );
            }
        }
    }

    // ---- comprehensions -----------------------------------------------------------------

    /// `[element for x in items if cond]` and `{k: v for ...}`: a loop that fills a new list
    /// or dict. The loop variables belong to the comprehension.
    pub(super) fn comprehension(
        &mut self,
        cx: &mut FnCx,
        element: &ast::Expr,
        kind: CompKind,
        clauses: &[ast::CompClause],
        span: Span,
        expected: Option<Ty>,
    ) -> hir::Expr {
        let (value, as_set) = match kind {
            CompKind::List => (None, false),
            CompKind::Set => (None, true),
            CompKind::Dict(value) => (Some(value), false),
        };
        let saved_scope = cx.scope.clone();
        let mut loops: Vec<(hir::LoopTarget, hir::LoopSource, Vec<hir::Expr>)> = Vec::new();
        let mut failed = false;
        for clause in clauses {
            let (source, elem) = self.loop_source(cx, &clause.iter);
            let mut names = Vec::new();
            target_names(&clause.target, &mut names);
            for name in &names {
                cx.scope.remove(name);
            }
            let target = self.loop_target(cx, &clause.target, elem);
            for name in &names {
                if let Some(&local) = cx.scope.get(name) {
                    cx.info[local.0].read = true;
                }
            }
            let conditions: Vec<hir::Expr> = clause.conditions.iter().map(|c| self.condition(cx, c)).collect();
            match target {
                Some(target) => loops.push((target, source, conditions)),
                None => failed = true,
            }
        }
        let (result_ty, inner) = match value {
            None if as_set => {
                let expected_elem = expected.and_then(|t| self.types.set_elem(t));
                let item = match expected_elem {
                    Some(elem) => self.expect(cx, element, elem, None),
                    None => self.expr(cx, element, None),
                };
                if item.ty == Ty::NONE {
                    self.error(Diagnostic::error(
                        codes::CANNOT_INFER,
                        "the elements of this set have no value",
                        element.span,
                        "produces `None`",
                    ));
                    failed = true;
                } else if item.ty != Ty::ERROR && !self.hashable(item.ty) {
                    self.unhashable(item.ty, item.span);
                    failed = true;
                }
                let set_ty = self.types.set(item.ty);
                (set_ty, vec![item])
            }
            None => {
                let expected_elem = expected.and_then(|t| match self.types.kind(t) {
                    TyKind::List(elem) => Some(*elem),
                    _ => None,
                });
                let item = match expected_elem {
                    Some(elem) => self.expect(cx, element, elem, None),
                    None => self.expr(cx, element, None),
                };
                if item.ty == Ty::NONE {
                    self.error(Diagnostic::error(
                        codes::CANNOT_INFER,
                        "the elements of this list have no value",
                        element.span,
                        "produces `None`",
                    ));
                    failed = true;
                }
                let list_ty = self.types.list(item.ty);
                (list_ty, vec![item])
            }
            Some(value_expr) => {
                let expected_kv = expected.and_then(|t| match self.types.kind(t) {
                    TyKind::Dict(k, v) if *v != Ty::NONE => Some((*k, *v)),
                    _ => None,
                });
                let (key, value) = match expected_kv {
                    Some((k, v)) => (self.expect(cx, element, k, None), self.expect(cx, value_expr, v, None)),
                    None => (self.expr(cx, element, None), self.expr(cx, value_expr, None)),
                };
                if !self.hashable(key.ty) {
                    self.unhashable(key.ty, key.span);
                    failed = true;
                }
                if value.ty == Ty::NONE {
                    self.error(
                        Diagnostic::error(
                            codes::CANNOT_INFER,
                            "the values of this dict have no value",
                            value.span,
                            "produces `None`",
                        )
                        .with_help(
                            "for a set, write `{x for x in items}`; for a dict, give a value of a type such as `int?`",
                        ),
                    );
                    failed = true;
                }
                let dict_ty = self.types.dict(key.ty, value.ty);
                (dict_ty, vec![key, value])
            }
        };
        cx.scope = saved_scope;
        if failed || self.types.contains_error(result_ty) {
            return error_expr(span);
        }
        let temp = cx.temp(result_ty, span);
        let load = mk(K::Local(temp), result_ty, span);
        // A list comprehension with one `for` and no `if`, over a list or a `range` with step 1,
        // has as many elements as its source: the list starts with that much room. The source
        // is evaluated into temporaries first, once, as the loop would.
        let mut hoisted = Vec::new();
        let mut capacity = None;
        if value.is_none()
            && !as_set
            && let [(_, source, conditions)] = loops.as_mut_slice()
            && conditions.is_empty()
        {
            let mut hoist = |cx: &mut FnCx, expr: &mut hir::Expr| {
                let local = cx.temp(expr.ty, span);
                let original = std::mem::replace(expr, mk(K::Local(local), expr.ty, span));
                hoisted.push(hir::Stmt::Assign { place: hir::Place::Local(local), value: original });
                mk(K::Local(local), expr.ty, span)
            };
            match source {
                hir::LoopSource::List(list) => {
                    let list = hoist(cx, list);
                    capacity = Some(mk(K::Builtin { op: hir::Builtin::Len, args: vec![list] }, Ty::INT, span));
                }
                hir::LoopSource::Range { start, stop, step } if matches!(step.kind, K::Int(1)) => {
                    let start = hoist(cx, start);
                    let stop = hoist(cx, stop);
                    let op = hir::BinaryOp::IntSub;
                    capacity = Some(mk(K::Binary { op, lhs: Box::new(stop), rhs: Box::new(start) }, Ty::INT, span));
                }
                _ => {}
            }
        }
        let mut inner = inner.into_iter();
        let mut body = match value {
            None if as_set => {
                let item = inner.next().expect("an element");
                vec![hir::Stmt::Expr(mk(
                    K::Builtin { op: hir::Builtin::SetAdd, args: vec![load.clone(), item] },
                    Ty::NONE,
                    span,
                ))]
            }
            None => {
                let item = inner.next().expect("an element");
                vec![hir::Stmt::Expr(mk(
                    K::Builtin { op: hir::Builtin::ListAppend, args: vec![load.clone(), item] },
                    Ty::NONE,
                    span,
                ))]
            }
            Some(_) => {
                let key = inner.next().expect("a key");
                let value = inner.next().expect("a value");
                vec![hir::Stmt::Assign {
                    place: hir::Place::Index { container: load.clone(), index: key, span },
                    value,
                }]
            }
        };
        for (target, source, conditions) in loops.into_iter().rev() {
            if let Some(cond) =
                conditions.into_iter().reduce(|a, b| mk(K::And(Box::new(a), Box::new(b)), Ty::BOOL, span))
            {
                body = vec![hir::Stmt::If { cond, then: body, otherwise: Vec::new() }];
            }
            body = vec![hir::Stmt::For { target, source, body }];
        }
        let empty = match (value, capacity) {
            (None, Some(capacity)) => {
                mk(K::Builtin { op: hir::Builtin::ListWithCapacity, args: vec![capacity] }, result_ty, span)
            }
            (None, None) if as_set => mk(K::Dict(Vec::new()), result_ty, span),
            (None, None) => mk(K::List(Vec::new()), result_ty, span),
            (Some(_), _) => mk(K::Dict(Vec::new()), result_ty, span),
        };
        let mut stmts = hoisted;
        stmts.push(hir::Stmt::Assign { place: hir::Place::Local(temp), value: empty });
        stmts.extend(body);
        mk(K::Block { stmts, value: Box::new(load) }, result_ty, span)
    }

    // ---- narrowing ----------------------------------------------------------------------

    /// Optional locals known not to be `None` when `cond` is true (`positive`) or false.
    pub(super) fn narrowings(&self, cx: &FnCx, cond: &ast::Expr, positive: bool) -> Vec<LocalId> {
        use ast::BinaryOp as A;
        match &cond.kind {
            ast::ExprKind::Binary { op: op @ (A::Is | A::IsNot | A::Eq | A::NotEq), lhs, rhs } => {
                let other = if rhs.is_none_literal() {
                    lhs
                } else if lhs.is_none_literal() {
                    rhs
                } else {
                    return Vec::new();
                };
                let not_none_when_true = matches!(op, A::IsNot | A::NotEq);
                if not_none_when_true != positive {
                    return Vec::new();
                }
                let ast::ExprKind::Name(name) = &other.kind else { return Vec::new() };
                match cx.scope.get(name) {
                    Some(&local) if self.types.option_inner(cx.locals[local.0].ty).is_some() => vec![local],
                    _ => Vec::new(),
                }
            }
            ast::ExprKind::Binary { op: A::And, lhs, rhs } if positive => {
                let mut out = self.narrowings(cx, lhs, true);
                out.extend(self.narrowings(cx, rhs, true));
                out
            }
            ast::ExprKind::Binary { op: A::Or, lhs, rhs } if !positive => {
                let mut out = self.narrowings(cx, lhs, false);
                out.extend(self.narrowings(cx, rhs, false));
                out
            }
            ast::ExprKind::Unary { op: ast::UnaryOp::Not, operand } => self.narrowings(cx, operand, !positive),
            _ => Vec::new(),
        }
    }

    /// Stop narrowing the locals a loop or match body assigns, since they may hold `None` again.
    pub(super) fn unnarrow_assigned(&self, cx: &mut FnCx, stmts: &[ast::Stmt]) {
        let mut assigned = HashMap::new();
        prescan(stmts, &mut assigned);
        cx.narrowed.retain(|l| !assigned.contains_key(&cx.locals[l.0].name));
    }
}

fn target_names(target: &ast::Target, out: &mut Vec<String>) {
    match &target.kind {
        ast::TargetKind::Name(id) => out.push(id.name.clone()),
        ast::TargetKind::Tuple(items) => items.iter().for_each(|t| target_names(t, out)),
        _ => {}
    }
}
