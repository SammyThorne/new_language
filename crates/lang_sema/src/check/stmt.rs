//! Statements, control flow, and definite-assignment analysis.
//!
//! Each statement is lowered to zero or more HIR statements and reports whether it *diverges*:
//! whether control never reaches the next statement (`return`, `break`, `continue`, `panic`, an
//! `if` whose branches all diverge, or `while True` without `break`). Divergence drives the
//! unreachable-code warning, the missing-`return` error, and the merging of assignment state.

use super::{Checker, FnCx, Reason, plural};
use crate::hir::{self, LocalId};
use crate::types::{Ty, TyKind};
use lang_diag::{Diagnostic, Span, codes};
use lang_syntax::ast;

/// Assignment state after two paths join: assigned only if assigned on both.
pub(super) fn merge(a: &[bool], b: &[bool]) -> Vec<bool> {
    let n = a.len().max(b.len());
    (0..n).map(|i| a.get(i).copied().unwrap_or(false) && b.get(i).copied().unwrap_or(false)).collect()
}

impl Checker<'_, '_> {
    pub(super) fn block(&mut self, cx: &mut FnCx, stmts: &[ast::Stmt]) -> (hir::Block, bool) {
        let mut out = Vec::new();
        let mut diverged = false;
        let mut warned = false;
        for stmt in stmts {
            if diverged && !warned {
                warned = true;
                self.diags.push(
                    Diagnostic::warning(codes::UNREACHABLE_CODE, "unreachable code", stmt.span, "this never runs")
                        .with_help("it comes after a statement that always leaves this block"),
                );
            }
            let (lowered, diverges) = self.stmt(cx, stmt);
            if !diverged {
                out.extend(lowered);
            }
            diverged |= diverges;
        }
        (out, diverged)
    }

    fn stmt(&mut self, cx: &mut FnCx, stmt: &ast::Stmt) -> (Vec<hir::Stmt>, bool) {
        match &stmt.kind {
            ast::StmtKind::Expr(expr) => {
                let checked = self.expr(cx, expr, None);
                if self.types.result_parts(checked.ty).is_some() && !self.types.contains_error(checked.ty) {
                    self.error(
                        Diagnostic::error(
                            codes::UNUSED_RESULT,
                            "this `Result` is not used",
                            expr.span,
                            "may hold an error that is being ignored",
                        )
                        .with_help("pass the error on with `?`, handle it with `match`, or discard it with `_ = ...`"),
                    );
                }
                let diverges = matches!(checked.kind, hir::ExprKind::Builtin { op: hir::Builtin::Panic, .. });
                (vec![hir::Stmt::Expr(checked)], diverges)
            }
            ast::StmtKind::Match { subject, cases } => {
                let before = cx.narrowed.clone();
                let result = self.match_stmt(cx, subject, cases, stmt.span);
                cx.narrowed = before;
                for case in cases {
                    self.unnarrow_assigned(cx, &case.body);
                }
                result
            }
            ast::StmtKind::Def(function) => (self.nested_def(cx, function), false),
            ast::StmtKind::With { value, target, body } => self.with_stmt(cx, value, target.as_ref(), body),
            ast::StmtKind::Pass => (Vec::new(), false),
            ast::StmtKind::Del(target) => (self.del_stmt(cx, target), false),
            ast::StmtKind::Assign { target, ty, value } => (self.assign(cx, target, ty.as_ref(), value), false),
            ast::StmtKind::AugAssign { target, op, value } => (self.aug_assign(cx, target, *op, value), false),
            ast::StmtKind::Return(value) => (vec![self.return_stmt(cx, stmt.span, value.as_ref())], true),
            ast::StmtKind::If { branches, otherwise } => self.if_chain(cx, branches, otherwise.as_deref()),
            ast::StmtKind::While { cond, body } => self.while_loop(cx, cond, body),
            ast::StmtKind::For { target, iter, body } => self.for_loop(cx, target, iter, body),
            ast::StmtKind::Break | ast::StmtKind::Continue => {
                let is_break = matches!(stmt.kind, ast::StmtKind::Break);
                match cx.loops.last_mut() {
                    Some(has_break) => {
                        *has_break |= is_break;
                        (vec![if is_break { hir::Stmt::Break } else { hir::Stmt::Continue }], true)
                    }
                    None => {
                        let word = if is_break { "break" } else { "continue" };
                        self.error(Diagnostic::error(
                            codes::OUTSIDE_LOOP,
                            format!("`{word}` outside a loop"),
                            stmt.span,
                            "not inside `for` or `while`",
                        ));
                        (Vec::new(), false)
                    }
                }
            }
        }
    }

    /// `del xs[i]` removes a list element (the same operation as `xs.pop(i)`, with the value
    /// dropped), and `del d[key]` removes a dict entry, panicking when the key is absent, as
    /// reading it would.
    fn del_stmt(&mut self, cx: &mut FnCx, target: &ast::Expr) -> Vec<hir::Stmt> {
        let ast::ExprKind::Index { object, index } = &target.kind else {
            let (message, help) = match &target.kind {
                ast::ExprKind::Name(name) => (
                    format!("`del {name}` is not supported"),
                    "a variable cannot be removed; leave it alone, or reassign it".to_string(),
                ),
                ast::ExprKind::Attr { .. } => (
                    "`del` cannot remove a field".to_string(),
                    "a struct's fields always exist; give an optional field `None` instead".to_string(),
                ),
                ast::ExprKind::Slice { .. } => (
                    "`del` cannot remove a slice".to_string(),
                    "build the list without those elements, for example `xs = xs[:a] + xs[b:]`".to_string(),
                ),
                _ => (
                    "`del` needs a list element or a dict entry".to_string(),
                    "write `del xs[i]` or `del d[key]`".to_string(),
                ),
            };
            self.error(Diagnostic::error(codes::NOT_SUPPORTED, message, target.span, "not supported").with_help(help));
            return Vec::new();
        };
        let container = self.expr(cx, object, None);
        let op = match self.types.kind(container.ty).clone() {
            TyKind::List(elem) => {
                let index = self.expect(cx, index, Ty::INT, None);
                hir::Expr {
                    kind: hir::ExprKind::Builtin { op: hir::Builtin::ListPopAt, args: vec![container, index] },
                    ty: elem,
                    span: target.span,
                }
            }
            TyKind::Dict(_, Ty::NONE) => {
                self.error(
                    Diagnostic::error(
                        codes::BAD_INDEX,
                        "`del` cannot remove an element of a set",
                        target.span,
                        "a set has no positions",
                    )
                    .with_help("use `items.remove(x)`, which panics if it is absent, or `items.discard(x)`"),
                );
                self.expr(cx, index, None);
                return Vec::new();
            }
            TyKind::Dict(key, _) => {
                let index = self.expect(cx, index, key, None);
                hir::Expr {
                    kind: hir::ExprKind::Builtin { op: hir::Builtin::DictDelete, args: vec![container, index] },
                    ty: Ty::NONE,
                    span: target.span,
                }
            }
            TyKind::Error => {
                self.expr(cx, index, None);
                return Vec::new();
            }
            kind => {
                let shown = self.display(container.ty);
                let help = match kind {
                    TyKind::FrozenList(_) | TyKind::FrozenDict(..) | TyKind::Tuple(_) => {
                        "this value cannot be changed after it is made"
                    }
                    _ => "only lists and dicts have elements to remove",
                };
                self.error(
                    Diagnostic::error(
                        codes::BAD_INDEX,
                        format!("`del` cannot remove an element of `{shown}`"),
                        target.span,
                        format!("has type `{shown}`"),
                    )
                    .with_help(help),
                );
                return Vec::new();
            }
        };
        vec![hir::Stmt::Expr(op)]
    }

    /// `items[x] = ...` where `items` is a set.
    fn set_element_assignment(&mut self, span: Span) {
        self.error(
            Diagnostic::error(codes::BAD_INDEX, "cannot assign to an element of a set", span, "a set has no positions")
                .with_help("add an element with `items.add(x)`"),
        );
    }

    /// `with value as name:`. The value's type needs `def exit(self)`; with `def enter(self)`,
    /// `name` is bound to what `enter` returns, and otherwise to the value itself.
    fn with_stmt(
        &mut self,
        cx: &mut FnCx,
        value: &ast::Expr,
        target: Option<&ast::Ident>,
        body: &[ast::Stmt],
    ) -> (Vec<hir::Stmt>, bool) {
        // `with m.lock() as value:` on a mutex.
        let checked = match &value.kind {
            ast::ExprKind::Call { callee, args } if args.is_empty() => match &callee.kind {
                ast::ExprKind::Attr { object, name } if name.name == "lock" => {
                    let receiver = self.expr(cx, object, None);
                    if matches!(self.types.kind(receiver.ty), TyKind::Mutex(_)) {
                        return self.lock_block(cx, receiver, target, body, value.span);
                    }
                    self.method_call(cx, receiver, name, args, value.span)
                }
                _ => self.expr(cx, value, None),
            },
            _ => self.expr(cx, value, None),
        };
        let ty = checked.ty;
        let exit = self.method_of(ty, "exit").filter(|f| self.signatures[f.0].ret == Ty::NONE);
        let enter = self.method_of(ty, "enter");
        let Some(exit) = exit else {
            if ty != Ty::ERROR {
                let shown = self.display(ty);
                let help = if self.types.result_parts(ty).is_some() {
                    "this is a `Result`; get the value first, for example with `?`".to_string()
                } else {
                    "a value works with `with` when its type has a method `def exit(self)`, which runs when the block ends"
                        .to_string()
                };
                self.error(
                    Diagnostic::error(
                        codes::BAD_WITH,
                        format!("a value of type `{shown}` cannot be used with `with`"),
                        value.span,
                        format!("has type `{shown}`"),
                    )
                    .with_help(help),
                );
            }
            if let Some(name) = target {
                let local = cx.add_local(&name.name, Ty::ERROR, name.span, false);
                cx.set_assigned(local);
            }
            let (_, diverges) = self.block(cx, body);
            return (Vec::new(), diverges);
        };
        let temp = cx.temp(ty, value.span);
        let load = hir::Expr { kind: hir::ExprKind::Local(temp), ty, span: value.span };
        let bound = match enter {
            Some(enter) => hir::Expr {
                kind: hir::ExprKind::Call { func: enter, args: vec![load] },
                ty: self.signatures[enter.0].ret,
                span: value.span,
            },
            None => load,
        };
        let mut inner = Vec::new();
        match target {
            Some(name) => {
                let local = match cx.scope.get(&name.name) {
                    Some(&local) => {
                        let declared = cx.locals[local.0].ty;
                        let bound = self.coerce(bound, declared, None);
                        inner.push(hir::Stmt::Assign { place: hir::Place::Local(local), value: bound });
                        local
                    }
                    None => {
                        let local = cx.add_local(&name.name, bound.ty, name.span, false);
                        inner.push(hir::Stmt::Assign { place: hir::Place::Local(local), value: bound });
                        local
                    }
                };
                cx.set_assigned(local);
            }
            None if enter.is_some() => inner.push(hir::Stmt::Expr(bound)),
            None => {}
        }
        let (body, diverges) = self.block(cx, body);
        inner.extend(body);
        let stmts = vec![
            hir::Stmt::Assign { place: hir::Place::Local(temp), value: checked },
            hir::Stmt::With { object: temp, exit: hir::WithExit::Method(exit), body: inner },
        ];
        (stmts, diverges)
    }

    /// A condition of `if`, `elif` or `while`.
    pub(super) fn condition(&mut self, cx: &mut FnCx, cond: &ast::Expr) -> hir::Expr {
        let checked = self.expr(cx, cond, Some(Ty::BOOL));
        if !self.types.fits(checked.ty, Ty::BOOL) {
            let shown = self.display(checked.ty);
            let help = if self.types.option_inner(checked.ty).is_some() {
                "check for a value explicitly: `x is not None`"
            } else {
                "values are not true or false by themselves; compare explicitly, for example `len(items) > 0`"
            };
            self.error(
                Diagnostic::error(
                    codes::NOT_BOOL,
                    format!("a condition must be a `bool`, found `{shown}`"),
                    cond.span,
                    format!("has type `{shown}`"),
                )
                .with_help(help),
            );
        }
        checked
    }

    fn if_chain(
        &mut self,
        cx: &mut FnCx,
        branches: &[(ast::Expr, Vec<ast::Stmt>)],
        otherwise: Option<&[ast::Stmt]>,
    ) -> (Vec<hir::Stmt>, bool) {
        let (cond_ast, body) = &branches[0];
        let cond = self.condition(cx, cond_ast);
        // Flow narrowing: `x is not None` makes `x` a plain value in the branch where it holds.
        let when_true = self.narrowings(cx, cond_ast, true);
        let when_false = self.narrowings(cx, cond_ast, false);
        let narrowed_before = cx.narrowed.clone();
        let before = cx.assigned.clone();
        cx.narrowed.extend(when_true);
        let (then, then_diverges) = self.block(cx, body);
        let then_state = std::mem::replace(&mut cx.assigned, before);
        let then_narrowed = std::mem::replace(&mut cx.narrowed, narrowed_before.clone());
        cx.narrowed.extend(when_false);
        let (otherwise, else_diverges) = if branches.len() > 1 {
            self.if_chain(cx, &branches[1..], otherwise)
        } else if let Some(body) = otherwise {
            self.block(cx, body)
        } else {
            (Vec::new(), false)
        };
        let else_state = std::mem::take(&mut cx.assigned);
        let else_narrowed = std::mem::take(&mut cx.narrowed);
        cx.assigned = match (then_diverges, else_diverges) {
            (true, true) | (false, true) => then_state,
            (true, false) => else_state,
            (false, false) => merge(&then_state, &else_state),
        };
        cx.narrowed = match (then_diverges, else_diverges) {
            (true, true) => narrowed_before,
            (false, true) => then_narrowed,
            (true, false) => else_narrowed,
            (false, false) => then_narrowed.into_iter().filter(|l| else_narrowed.contains(l)).collect(),
        };
        (vec![hir::Stmt::If { cond, then, otherwise }], then_diverges && else_diverges)
    }

    fn while_loop(&mut self, cx: &mut FnCx, cond: &ast::Expr, body: &[ast::Stmt]) -> (Vec<hir::Stmt>, bool) {
        let infinite = matches!(cond.kind, ast::ExprKind::Bool(true));
        // Narrowing holds inside the loop only for variables the loop does not assign.
        self.unnarrow_assigned(cx, body);
        let cond_ast = cond;
        let cond = self.condition(cx, cond);
        let before = cx.assigned.clone();
        let narrowed_before = cx.narrowed.clone();
        cx.narrowed.extend(self.narrowings(cx, cond_ast, true));
        cx.loops.push(false);
        let (body, _) = self.block(cx, body);
        let has_break = cx.loops.pop().unwrap_or(false);
        // The body may run zero times, so nothing it assigns counts afterwards.
        cx.assigned = before;
        cx.narrowed = narrowed_before;
        (vec![hir::Stmt::While { cond, body }], infinite && !has_break)
    }

    fn for_loop(
        &mut self,
        cx: &mut FnCx,
        target: &ast::Target,
        iter: &ast::Expr,
        body: &[ast::Stmt],
    ) -> (Vec<hir::Stmt>, bool) {
        let (source, elem) = self.loop_source(cx, iter);
        self.unnarrow_assigned(cx, body);
        let before = cx.assigned.clone();
        let narrowed_before = cx.narrowed.clone();
        let target = self.loop_target(cx, target, elem);
        cx.loops.push(false);
        let (body, _) = self.block(cx, body);
        cx.loops.pop();
        cx.assigned = before;
        cx.narrowed = narrowed_before;
        match target {
            Some(target) => (vec![hir::Stmt::For { target, source, body }], false),
            None => (Vec::new(), false),
        }
    }

    /// What a `for` loop iterates over, and the element type.
    pub(super) fn loop_source(&mut self, cx: &mut FnCx, iter: &ast::Expr) -> (hir::LoopSource, Ty) {
        if let ast::ExprKind::Call { callee, args } = &iter.kind {
            if let ast::ExprKind::Name(name) = &callee.kind
                && name == "range"
                && !cx.sees("range")
            {
                return (self.range(cx, args, iter.span), Ty::INT);
            }
            // `d.keys()`, `d.values()` and `d.items()` are iterated in place, without building
            // a list.
            if let ast::ExprKind::Attr { object, name } = &callee.kind
                && args.is_empty()
                && matches!(name.name.as_str(), "keys" | "values" | "items")
            {
                let receiver = self.expr(cx, object, None);
                if let TyKind::Dict(key, value) | TyKind::FrozenDict(key, value) = *self.types.kind(receiver.ty)
                    && value != Ty::NONE
                {
                    return match name.name.as_str() {
                        "keys" => (hir::LoopSource::DictKeys(receiver), key),
                        "values" => (hir::LoopSource::DictValues(receiver), value),
                        _ => {
                            let pair = self.types.tuple(vec![key, value]);
                            (hir::LoopSource::DictItems(receiver), pair)
                        }
                    };
                }
                let checked = self.method_call(cx, receiver, name, args, iter.span);
                return self.iterate(checked, iter.span);
            }
        }
        let checked = self.expr(cx, iter, None);
        self.iterate(checked, iter.span)
    }

    fn iterate(&mut self, iterable: hir::Expr, span: Span) -> (hir::LoopSource, Ty) {
        match self.types.kind(iterable.ty).clone() {
            TyKind::List(elem) => (hir::LoopSource::List(iterable), elem),
            TyKind::Str => (hir::LoopSource::Str(iterable), Ty::STR),
            TyKind::Dict(key, _) => (hir::LoopSource::DictKeys(iterable), key),
            TyKind::Error => (hir::LoopSource::List(iterable), Ty::ERROR),
            TyKind::FrozenList(elem) => (hir::LoopSource::List(iterable), elem),
            TyKind::FrozenDict(key, _) => (hir::LoopSource::DictKeys(iterable), key),
            TyKind::Channel(elem) => (hir::LoopSource::Channel(iterable), elem),
            TyKind::Struct(_) | TyKind::Enum(_) if self.iterator_parts(iterable.ty).is_some() => {
                // An iterator: a type with `def next(self) -> T?`.
                let (next, elem) = self.iterator_parts(iterable.ty).expect("checked");
                (hir::LoopSource::Iterator { iterator: iterable, next }, elem)
            }
            TyKind::Struct(_) | TyKind::Enum(_) if self.method_of(iterable.ty, "iter").is_some() => {
                // An iterable: a type with `def iter(self)` returning an iterator.
                let iter = self.method_of(iterable.ty, "iter").expect("checked");
                let iterator_ty = self.signatures[iter.0].ret;
                let Some((next, elem)) = self.iterator_parts(iterator_ty) else {
                    let (shown, it) = (self.display(iterable.ty), self.display(iterator_ty));
                    self.error(
                        Diagnostic::error(
                            codes::NOT_ITERABLE,
                            format!(
                                "cannot loop over `{shown}`: its `iter` method returns `{it}`, which is not an iterator"
                            ),
                            span,
                            "not iterable",
                        )
                        .with_help(
                            "an iterator has a method `def next(self) -> T?` that returns `None` when it is finished",
                        ),
                    );
                    return (hir::LoopSource::List(iterable), Ty::ERROR);
                };
                let call =
                    hir::Expr { kind: hir::ExprKind::Call { func: iter, args: vec![iterable] }, ty: iterator_ty, span };
                (hir::LoopSource::Iterator { iterator: call, next }, elem)
            }
            _ => {
                let shown = self.display(iterable.ty);
                let help = if iterable.ty == Ty::INT {
                    "to count, loop over `range(n)`"
                } else {
                    "loops work over `range(...)`, lists, strings, dicts, and types with `def next(self) -> T?` or `def iter(self)`"
                };
                self.error(
                    Diagnostic::error(
                        codes::NOT_ITERABLE,
                        format!("cannot loop over a value of type `{shown}`"),
                        span,
                        format!("has type `{shown}`"),
                    )
                    .with_help(help),
                );
                (hir::LoopSource::List(iterable), Ty::ERROR)
            }
        }
    }

    fn range(&mut self, cx: &mut FnCx, args: &[ast::Arg], span: Span) -> hir::LoopSource {
        if let Some(arg) = args.iter().find(|a| a.name.is_some()) {
            let span = arg.value.span;
            self.error(Diagnostic::error(
                codes::NOT_SUPPORTED,
                "`range` takes no keyword arguments",
                span,
                "remove the name",
            ));
        }
        let values: Vec<hir::Expr> = args.iter().map(|a| self.expect(cx, &a.value, Ty::INT, None)).collect();
        let int = |value: i64| hir::Expr { kind: hir::ExprKind::Int(value), ty: Ty::INT, span };
        let mut values = values.into_iter();
        match args.len() {
            1 => hir::LoopSource::Range { start: int(0), stop: values.next().unwrap(), step: int(1) },
            2 => hir::LoopSource::Range { start: values.next().unwrap(), stop: values.next().unwrap(), step: int(1) },
            3 => hir::LoopSource::Range {
                start: values.next().unwrap(),
                stop: values.next().unwrap(),
                step: values.next().unwrap(),
            },
            n => {
                self.error(Diagnostic::error(
                    codes::WRONG_ARG_COUNT,
                    format!("`range` takes 1 to 3 arguments but {n} were given"),
                    span,
                    "range(stop), range(start, stop) or range(start, stop, step)",
                ));
                hir::LoopSource::Range { start: int(0), stop: int(0), step: int(1) }
            }
        }
    }

    /// A method taking `self` with no other parameters, from the type's own methods or a
    /// trait implementation.
    pub(super) fn method_of(&self, ty: Ty, name: &str) -> Option<crate::hir::FuncId> {
        let func = match self.inherent.get(&(ty, name.to_string())) {
            Some(&func) => Some(func),
            None => match self.trait_methods.get(&(ty, name.to_string())).map(Vec::as_slice) {
                Some([(trait_id, index)]) => self.impl_of(*trait_id, ty).and_then(|i| self.impls[i].methods[*index]),
                _ => None,
            },
        }?;
        let signature = &self.signatures[func.0];
        (signature.receiver.is_some() && signature.params.is_empty()).then_some(func)
    }

    /// For an iterator type: its `next` method and the element type.
    fn iterator_parts(&self, ty: Ty) -> Option<(crate::hir::FuncId, Ty)> {
        let next = self.method_of(ty, "next")?;
        let elem = self.types.option_inner(self.signatures[next.0].ret)?;
        Some((next, elem))
    }

    pub(super) fn loop_target(&mut self, cx: &mut FnCx, target: &ast::Target, elem: Ty) -> Option<hir::LoopTarget> {
        match &target.kind {
            ast::TargetKind::Name(name) => Some(hir::LoopTarget::Local(self.bind_loop_name(cx, name, elem)?)),
            ast::TargetKind::Tuple(items) => {
                let elem_types = match self.types.kind(elem).clone() {
                    TyKind::Tuple(types) if types.len() == items.len() => types,
                    TyKind::Error => vec![Ty::ERROR; items.len()],
                    _ => {
                        self.declare_failed_targets(cx, items);
                        let shown = self.display(elem);
                        self.error(Diagnostic::error(
                            codes::UNPACK_MISMATCH,
                            format!("cannot unpack `{shown}` into {}", plural(items.len(), "name")),
                            target.span,
                            format!("each element is `{shown}`"),
                        ));
                        return None;
                    }
                };
                let mut locals = Vec::new();
                for (item, ty) in items.iter().zip(elem_types) {
                    let ast::TargetKind::Name(name) = &item.kind else {
                        self.error(Diagnostic::error(
                            codes::NOT_SUPPORTED,
                            "loop variables must be names",
                            item.span,
                            "not a name",
                        ));
                        return None;
                    };
                    locals.push(self.bind_loop_name(cx, name, ty)?);
                }
                Some(hir::LoopTarget::Tuple(locals))
            }
            _ => {
                self.error(Diagnostic::error(
                    codes::NOT_SUPPORTED,
                    "loop variables must be names",
                    target.span,
                    "not a name",
                ));
                None
            }
        }
    }

    fn bind_loop_name(&mut self, cx: &mut FnCx, name: &ast::Ident, ty: Ty) -> Option<LocalId> {
        if !cx.scope.contains_key(&name.name)
            && let Some(kind) = self.local_conflict(&name.name)
        {
            self.error(Diagnostic::error(
                codes::NOT_A_FUNCTION,
                format!("cannot use `{}` as a loop variable; it is {kind}", name.name),
                name.span,
                "already defined",
            ));
            return None;
        }
        let local = match cx.scope.get(&name.name) {
            Some(&local) => {
                let declared = cx.locals[local.0].ty;
                if !self.types.fits(ty, declared) {
                    let (declared_shown, found) = (self.display(declared), self.display(ty));
                    let decl_span = cx.info[local.0].decl_span;
                    self.error(
                        Diagnostic::error(
                            codes::TYPE_MISMATCH,
                            format!("loop variable `{}` already has type `{declared_shown}`", name.name),
                            name.span,
                            format!("the loop gives `{found}`"),
                        )
                        .with_label(decl_span, format!("`{}` was declared as `{declared_shown}` here", name.name)),
                    );
                }
                local
            }
            None => cx.add_local(&name.name, ty, name.span, false),
        };
        cx.set_assigned(local);
        Some(local)
    }

    fn return_stmt(&mut self, cx: &mut FnCx, span: Span, value: Option<&ast::Expr>) -> hir::Stmt {
        let (ret, ret_span) = (cx.ret, cx.ret_span);
        match (value, ret) {
            (None, Ty::NONE | Ty::ERROR) => hir::Stmt::Return(None),
            (None, ret) if self.returns_ok_none(ret) => hir::Stmt::Return(Some(self.ok_none(ret, span))),
            (None, ret) => {
                let shown = self.display(ret);
                let mut diagnostic = Diagnostic::error(
                    codes::TYPE_MISMATCH,
                    format!("`{}` must return a value of type `{shown}`", cx.name),
                    span,
                    "returns no value",
                );
                if let Some(ret_span) = ret_span {
                    diagnostic = diagnostic.with_label(ret_span, "return type declared here");
                }
                self.error(diagnostic);
                hir::Stmt::Return(None)
            }
            (Some(expr), Ty::NONE) => {
                let checked = self.expr(cx, expr, None);
                if !matches!(checked.ty, Ty::NONE | Ty::ERROR) {
                    let found = self.display(checked.ty);
                    self.error(
                        Diagnostic::error(
                            codes::RETURN_VALUE_FROM_NONE,
                            format!("function `{}` has no return type but returns a value", cx.name),
                            expr.span,
                            format!("returns `{found}`"),
                        )
                        .with_help(format!("declare the return type in the signature: `-> {found}`")),
                    );
                }
                hir::Stmt::Return(Some(checked))
            }
            (Some(expr), ret) => {
                let reason = ret_span.map(|s| (s, "expected because of this return type".to_string()));
                let value = self.expect(cx, expr, ret, reason);
                self.check_escape(cx, &value, "returned");
                hir::Stmt::Return(Some(value))
            }
        }
    }

    // ---- assignment ---------------------------------------------------------------------

    fn assign(
        &mut self,
        cx: &mut FnCx,
        target: &ast::Target,
        hint: Option<&ast::TypeExpr>,
        value: &ast::Expr,
    ) -> Vec<hir::Stmt> {
        match &target.kind {
            // `_ = value` evaluates and discards, which is how an unused `Result` is ignored.
            ast::TargetKind::Name(name) if name.name == "_" && hint.is_none() => {
                vec![hir::Stmt::Expr(self.expr(cx, value, None))]
            }
            ast::TargetKind::Name(name) => self.assign_name(cx, name, hint, value),
            ast::TargetKind::Tuple(targets) => {
                let checked = self.expr(cx, value, None);
                self.unpack(cx, targets, target.span, checked)
            }
            _ => {
                if let Some(hint) = hint {
                    self.error(Diagnostic::error(
                        codes::UNEXPECTED_TOKEN,
                        "a type hint is only allowed when declaring a variable",
                        hint.span,
                        "remove this",
                    ));
                }
                let Some((place, ty, reason)) = self.place(cx, target) else {
                    self.expr(cx, value, None);
                    return Vec::new();
                };
                let value = self.expect(cx, value, ty, reason);
                self.check_escape(cx, &value, "stored in a field or collection");
                vec![hir::Stmt::Assign { place, value }]
            }
        }
    }

    fn assign_name(
        &mut self,
        cx: &mut FnCx,
        name: &ast::Ident,
        hint: Option<&ast::TypeExpr>,
        value: &ast::Expr,
    ) -> Vec<hir::Stmt> {
        if !cx.scope.contains_key(&name.name)
            && let Some(kind) = self.local_conflict(&name.name)
        {
            self.error(Diagnostic::error(
                codes::NOT_A_FUNCTION,
                format!("cannot assign to `{}`, which is {kind}", name.name),
                name.span,
                format!("is {kind}"),
            ));
            return vec![hir::Stmt::Expr(self.expr(cx, value, None))];
        }

        if !cx.scope.contains_key(&name.name) && cx.sees(&name.name) {
            self.error(
                Diagnostic::error(
                    codes::ASSIGN_CAPTURED,
                    format!("cannot assign to `{}`, which belongs to the enclosing function", name.name),
                    name.span,
                    "assigned inside a nested function",
                )
                .with_help("a nested function can read the enclosing function's variables but not reassign them; keep changing state in a list or a struct field"),
            );
            return vec![hir::Stmt::Expr(self.expr(cx, value, None))];
        }
        if let Some(&local) = cx.scope.get(&name.name) {
            let declared = cx.locals[local.0].ty;
            let decl_span = cx.info[local.0].decl_span;
            let shown = self.display(declared);
            if let Some(hint) = hint {
                self.error(
                    Diagnostic::error(
                        codes::REDECLARED_VARIABLE,
                        format!("`{}` already has a type", name.name),
                        hint.span,
                        "a type hint is only allowed on the first assignment",
                    )
                    .with_label(decl_span, format!("`{}` was declared as `{shown}` here", name.name))
                    .with_help("remove the type hint"),
                );
            }
            let reason = Some((decl_span, format!("`{}` was declared as `{shown}` here", name.name)));
            let value = self.expect(cx, value, declared, reason);
            if !cx.guarded.contains(&local) {
                self.check_escape(cx, &value, "stored in another variable");
            }
            cx.expired.retain(|l| *l != local);
            if cx.locals[local.0].capture.is_some() {
                self.error(Diagnostic::error(
                    codes::ASSIGN_CAPTURED,
                    format!("cannot assign to `{}`, which belongs to the enclosing function", name.name),
                    name.span,
                    "assigned inside a nested function",
                ));
            }
            // Assigning a plain value keeps a narrowed optional narrowed; anything else ends it.
            if !matches!(value.kind, hir::ExprKind::Some(_)) {
                cx.narrowed.retain(|l| *l != local);
            }
            cx.set_assigned(local);
            return vec![hir::Stmt::Assign { place: hir::Place::Local(local), value }];
        }

        let (ty, value) = match hint {
            Some(hint) => {
                let declared = self.resolve_type(hint);
                if declared == Ty::NONE {
                    self.none_variable(&name.name, hint.span, "a variable of type `None` carries no information");
                }
                let reason = Some((hint.span, "expected because of this type hint".to_string()));
                (declared, self.expect(cx, value, declared, reason))
            }
            None => {
                let checked = self.expr(cx, value, None);
                if checked.ty == Ty::NONE {
                    let label = if value.is_none_literal() {
                        "the type cannot be inferred from `None`"
                    } else {
                        "this produces no value"
                    };
                    self.none_variable(&name.name, value.span, label);
                    (Ty::ERROR, checked)
                } else {
                    (checked.ty, checked)
                }
            }
        };
        self.check_escape(cx, &value, "stored in another variable");
        let local = cx.add_local(&name.name, ty, name.span, false);
        cx.set_assigned(local);
        vec![hir::Stmt::Assign { place: hir::Place::Local(local), value }]
    }

    fn none_variable(&mut self, name: &str, span: Span, label: &str) {
        let mut diagnostic =
            Diagnostic::error(codes::NONE_VARIABLE, format!("`{name}` would have type `None`"), span, label);
        if label.contains("inferred") {
            diagnostic = diagnostic.with_help(format!("give it an optional type: `{name}: int? = None`"));
        }
        self.error(diagnostic);
    }

    /// After a failed unpacking, declare the names anyway (with the error type) so that their
    /// uses do not produce follow-on errors.
    fn declare_failed_targets(&mut self, cx: &mut FnCx, targets: &[ast::Target]) {
        for target in targets {
            if let ast::TargetKind::Name(name) = &target.kind
                && self.local_conflict(&name.name).is_none()
            {
                let local = match cx.scope.get(&name.name) {
                    Some(&local) => local,
                    None => cx.add_local(&name.name, Ty::ERROR, name.span, false),
                };
                cx.info[local.0].read = true;
                cx.set_assigned(local);
            }
        }
    }

    /// `a, b = value`: evaluate once into a temporary, then assign each element.
    fn unpack(&mut self, cx: &mut FnCx, targets: &[ast::Target], span: Span, value: hir::Expr) -> Vec<hir::Stmt> {
        let elem_types = match self.types.kind(value.ty).clone() {
            TyKind::Tuple(types) if types.len() == targets.len() => types,
            TyKind::Error => {
                self.declare_failed_targets(cx, targets);
                return Vec::new();
            }
            _ => {
                self.declare_failed_targets(cx, targets);
                let shown = self.display(value.ty);
                self.error(
                    Diagnostic::error(
                        codes::UNPACK_MISMATCH,
                        format!("cannot unpack `{shown}` into {}", plural(targets.len(), "name")),
                        span,
                        format!("the value is `{shown}`"),
                    )
                    .with_help("unpacking needs a tuple with exactly as many elements as names"),
                );
                return Vec::new();
            }
        };
        let value_span = value.span;
        let temp = cx.temp(value.ty, value_span);
        let mut stmts = vec![hir::Stmt::Assign { place: hir::Place::Local(temp), value: value.clone() }];
        for (index, (target, ty)) in targets.iter().zip(elem_types).enumerate() {
            let element = hir::Expr {
                kind: hir::ExprKind::TupleGet {
                    tuple: Box::new(hir::Expr { kind: hir::ExprKind::Local(temp), ty: value.ty, span: value_span }),
                    index,
                },
                ty,
                span: target.span,
            };
            match &target.kind {
                ast::TargetKind::Name(name) => {
                    if !cx.scope.contains_key(&name.name)
                        && let Some(kind) = self.local_conflict(&name.name)
                    {
                        self.error(Diagnostic::error(
                            codes::NOT_A_FUNCTION,
                            format!("cannot assign to `{}`, which is {kind}", name.name),
                            name.span,
                            format!("is {kind}"),
                        ));
                        continue;
                    }
                    let local = match cx.scope.get(&name.name) {
                        Some(&local) => local,
                        None => cx.add_local(&name.name, ty, name.span, false),
                    };
                    let declared = cx.locals[local.0].ty;
                    let element = self.coerce(element, declared, None);
                    cx.set_assigned(local);
                    stmts.push(hir::Stmt::Assign { place: hir::Place::Local(local), value: element });
                }
                ast::TargetKind::Tuple(_) => {
                    self.error(Diagnostic::error(
                        codes::NOT_SUPPORTED,
                        "nested unpacking is not supported yet",
                        target.span,
                        "not supported yet",
                    ));
                }
                _ => {
                    if let Some((place, declared, _)) = self.place(cx, target) {
                        let element = self.coerce(element, declared, None);
                        stmts.push(hir::Stmt::Assign { place, value: element });
                    }
                }
            }
        }
        stmts
    }

    /// The place a field or element target refers to, its type, and what declared that type.
    fn place(&mut self, cx: &mut FnCx, target: &ast::Target) -> Option<(hir::Place, Ty, Reason)> {
        match &target.kind {
            ast::TargetKind::Attr { object, field } => {
                let object = self.expr(cx, object, None);
                let (index, ty, span) = self.field_of(&object, field)?;
                self.check_not_frozen(object.ty, field.span);
                Some((
                    hir::Place::Field { object, index },
                    ty,
                    Some((span, format!("field `{}` is declared here", field.name))),
                ))
            }
            ast::TargetKind::Index { object, index } => {
                let container = self.expr(cx, object, None);
                match self.types.kind(container.ty).clone() {
                    TyKind::List(elem) => {
                        let index = self.expect(cx, index, Ty::INT, None);
                        Some((hir::Place::Index { container, index, span: target.span }, elem, None))
                    }
                    TyKind::Dict(_, Ty::NONE) => {
                        self.set_element_assignment(target.span);
                        self.expr(cx, index, None);
                        None
                    }
                    TyKind::Dict(key, value) => {
                        let index = self.expect(cx, index, key, None);
                        Some((hir::Place::Index { container, index, span: target.span }, value, None))
                    }
                    TyKind::Error => None,
                    other => {
                        let shown = self.display(container.ty);
                        let what = match other {
                            TyKind::Str => "strings cannot be changed; build a new string instead",
                            TyKind::Tuple(_) => "tuples cannot be changed; build a new tuple instead",
                            _ => "only list and dict elements can be assigned",
                        };
                        self.error(Diagnostic::error(
                            codes::BAD_INDEX,
                            format!("cannot assign to an element of `{shown}`"),
                            target.span,
                            what,
                        ));
                        None
                    }
                }
            }
            _ => None,
        }
    }

    fn check_not_frozen(&mut self, ty: Ty, span: Span) {
        if let Some(id) = self.struct_of(ty)
            && self.structs[id.0].frozen
        {
            let (name, struct_span) = (self.structs[id.0].name.clone(), self.structs[id.0].span);
            self.error(
                Diagnostic::error(
                    codes::FROZEN_FIELD,
                    format!("cannot change a field of `{name}`, which is `@frozen`"),
                    span,
                    "frozen field",
                )
                .with_label(struct_span, format!("`{name}` is declared `@frozen` here"))
                .with_help("build a new value instead, or remove `@frozen` if the struct should be mutable"),
            );
        }
    }

    /// The index, type and declaration span of a struct field, reporting if there is none.
    pub(super) fn field_of(&mut self, object: &hir::Expr, field: &ast::Ident) -> Option<(usize, Ty, Span)> {
        if object.ty == Ty::ERROR {
            return None;
        }
        let Some(id) = self.struct_of(object.ty) else {
            let shown = self.display(object.ty);
            let optional_help;
            let help = match self.types.kind(object.ty) {
                TyKind::Option(inner) => {
                    let inner = self.display(*inner);
                    optional_help = format!(
                        "this value may be `None`; check `if x is not None:` first, or get the `{inner}` with `.unwrap()`"
                    );
                    Some(optional_help.as_str())
                }
                TyKind::Enum(_) => {
                    Some("enum values have no fields to read directly; use `match` to get at a variant's fields")
                }
                TyKind::SelfType(_) => Some(
                    "in a trait's default method, `self` can be any implementing type, so only the trait's methods are available",
                ),
                TyKind::Dyn(_) => Some("a trait object only offers the trait's methods"),
                _ => None,
            };
            let mut diagnostic = Diagnostic::error(
                codes::NO_SUCH_MEMBER,
                format!("`{shown}` has no field `{}`", field.name),
                field.span,
                "no such field",
            );
            if let Some(help) = help {
                diagnostic = diagnostic.with_help(help);
            }
            self.error(diagnostic);
            return None;
        };
        let info = &self.structs[id.0];
        if let Some(index) = info.fields.iter().position(|f| f.name == field.name) {
            let (ty, span) = (info.fields[index].ty, info.fields[index].span);
            self.check_field_visible(object.ty, index, field.span);
            return Some((index, ty, span));
        }
        let struct_name = info.name.clone();
        let struct_span = info.span;
        let ty = self.structs[id.0].ty;
        let is_method = self.inherent.contains_key(&(ty, field.name.clone()))
            || self.trait_methods.contains_key(&(ty, field.name.clone()));
        let info = &self.structs[id.0];
        let candidates: Vec<String> = info.fields.iter().map(|f| f.name.clone()).collect();
        let mut diagnostic = Diagnostic::error(
            codes::NO_SUCH_MEMBER,
            format!("`{struct_name}` has no field `{}`", field.name),
            field.span,
            if is_method { "this is a method" } else { "no such field" },
        )
        .with_label(struct_span, format!("`{struct_name}` is defined here"));
        if is_method {
            diagnostic = diagnostic.with_help(format!("call the method: `.{}(...)`", field.name));
        } else if let Some(suggestion) = super::closest(&field.name, candidates.iter().map(String::as_str)) {
            diagnostic = diagnostic.with_help(format!("did you mean `{suggestion}`?"));
        }
        self.error(diagnostic);
        None
    }

    fn aug_assign(
        &mut self,
        cx: &mut FnCx,
        target: &ast::Target,
        op: ast::BinaryOp,
        value: &ast::Expr,
    ) -> Vec<hir::Stmt> {
        let span = target.span.to(value.span);
        match &target.kind {
            ast::TargetKind::Name(name) => {
                if !cx.scope.contains_key(&name.name) && cx.sees(&name.name) {
                    self.error(
                        Diagnostic::error(
                            codes::ASSIGN_CAPTURED,
                            format!("cannot assign to `{}`, which belongs to the enclosing function", name.name),
                            name.span,
                            "assigned inside a nested function",
                        )
                        .with_help("a nested function can read the enclosing function's variables but not reassign them; keep changing state in a list or a struct field"),
                    );
                    self.expr(cx, value, None);
                    return Vec::new();
                }
                if !cx.scope.contains_key(&name.name) {
                    if let Some(kind) = self.local_conflict(&name.name) {
                        self.error(Diagnostic::error(
                            codes::NOT_A_FUNCTION,
                            format!("cannot assign to `{}`, which is {kind}", name.name),
                            name.span,
                            format!("is {kind}"),
                        ));
                    } else {
                        self.error(
                            Diagnostic::error(
                                codes::UNKNOWN_NAME,
                                format!("`{}` must be assigned before `{}=`", name.name, op.symbol()),
                                name.span,
                                "not assigned yet",
                            )
                            .with_help(format!("give it a starting value first: `{} = 0`", name.name)),
                        );
                    }
                    self.expr(cx, value, None);
                    return Vec::new();
                }
                let current = self.name_expr(cx, &name.name, name.span);
                let local = cx.scope[&name.name];
                if cx.locals[local.0].capture.is_some() {
                    self.error(Diagnostic::error(
                        codes::ASSIGN_CAPTURED,
                        format!("cannot assign to `{}`, which belongs to the enclosing function", name.name),
                        name.span,
                        "assigned inside a nested function",
                    ));
                }
                let declared = cx.locals[local.0].ty;
                let result = self.binary_with(cx, op, current, value, span);
                let result = self.coerce(result, declared, None);
                vec![hir::Stmt::Assign { place: hir::Place::Local(local), value: result }]
            }
            ast::TargetKind::Attr { object, field } => {
                // Evaluate the object once.
                let object = self.expr(cx, object, None);
                let Some((index, field_ty, _)) = self.field_of(&object, field) else {
                    self.expr(cx, value, None);
                    return Vec::new();
                };
                self.check_not_frozen(object.ty, field.span);
                let temp = cx.temp(object.ty, object.span);
                let object_ty = object.ty;
                let load = |span: Span| hir::Expr { kind: hir::ExprKind::Local(temp), ty: object_ty, span };
                let current = hir::Expr {
                    kind: hir::ExprKind::Field { object: Box::new(load(field.span)), index },
                    ty: field_ty,
                    span: target.span,
                };
                let result = self.binary_with(cx, op, current, value, span);
                let result = self.coerce(result, field_ty, None);
                vec![
                    hir::Stmt::Assign { place: hir::Place::Local(temp), value: object },
                    hir::Stmt::Assign { place: hir::Place::Field { object: load(field.span), index }, value: result },
                ]
            }
            ast::TargetKind::Index { object, index } => {
                // Evaluate the container and the index once each.
                let container = self.expr(cx, object, None);
                let (key_ty, elem_ty) = match self.types.kind(container.ty).clone() {
                    TyKind::List(elem) => (Ty::INT, elem),
                    TyKind::Dict(_, Ty::NONE) => {
                        self.set_element_assignment(target.span);
                        self.expr(cx, index, None);
                        self.expr(cx, value, None);
                        return Vec::new();
                    }
                    TyKind::Dict(key, value) => (key, value),
                    TyKind::Error => {
                        self.expr(cx, value, None);
                        return Vec::new();
                    }
                    _ => {
                        let shown = self.display(container.ty);
                        self.error(Diagnostic::error(
                            codes::BAD_INDEX,
                            format!("cannot assign to an element of `{shown}`"),
                            target.span,
                            "only list and dict elements can be assigned",
                        ));
                        self.expr(cx, value, None);
                        return Vec::new();
                    }
                };
                let index = self.expect(cx, index, key_ty, None);
                let (container_ty, container_span, index_span) = (container.ty, container.span, index.span);
                let container_temp = cx.temp(container_ty, container_span);
                let index_temp = cx.temp(key_ty, index_span);
                let load_container =
                    hir::Expr { kind: hir::ExprKind::Local(container_temp), ty: container_ty, span: container_span };
                let load_index = hir::Expr { kind: hir::ExprKind::Local(index_temp), ty: key_ty, span: index_span };
                let current = hir::Expr {
                    kind: hir::ExprKind::Index {
                        container: Box::new(load_container.clone()),
                        index: Box::new(load_index.clone()),
                    },
                    ty: elem_ty,
                    span: target.span,
                };
                let result = self.binary_with(cx, op, current, value, span);
                let result = self.coerce(result, elem_ty, None);
                vec![
                    hir::Stmt::Assign { place: hir::Place::Local(container_temp), value: container },
                    hir::Stmt::Assign { place: hir::Place::Local(index_temp), value: index },
                    hir::Stmt::Assign {
                        place: hir::Place::Index { container: load_container, index: load_index, span: target.span },
                        value: result,
                    },
                ]
            }
            ast::TargetKind::Tuple(_) => {
                self.expr(cx, value, None);
                Vec::new()
            }
        }
    }
}
