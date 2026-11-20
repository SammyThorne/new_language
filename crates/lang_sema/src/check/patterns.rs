//! `match`: checking patterns against the subject's type, binding names, and proving the cases
//! cover every value.
//!
//! Exhaustiveness uses the pattern-matrix "usefulness" algorithm (Maranget, "Warnings for pattern
//! matching", 2007): a pattern row is useful if some value matches it and no earlier row. A
//! `match` is exhaustive when a wildcard would not be useful after all its unguarded cases, and a
//! case is unreachable when it is not useful after the cases before it.

use super::{Checker, FnCx, Global, error_expr, plural};
use crate::hir::{self, LocalId, PatternKind as K};
use crate::types::{Ty, TyKind};
use lang_diag::{Diagnostic, Span, codes};
use lang_syntax::ast;
use std::collections::BTreeSet;

/// A pattern reduced to what exhaustiveness cares about.
#[derive(Clone, Debug)]
enum P {
    Wild,
    C(Ctor, Vec<P>),
    Or(Vec<P>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Ctor {
    Variant(usize),
    Bool(bool),
    Some,
    None,
    Ok,
    Err,
    Tuple,
    /// A literal of a type with infinitely many values; only a wildcard completes those.
    Literal(String),
}

fn simplify(pattern: &hir::Pattern) -> P {
    match &pattern.kind {
        K::Wild | K::Bind(_) => P::Wild,
        K::Literal(expr) => match &expr.kind {
            hir::ExprKind::Bool(b) => P::C(Ctor::Bool(*b), Vec::new()),
            other => P::C(Ctor::Literal(format!("{other:?}")), Vec::new()),
        },
        K::Variant { index, fields } => P::C(Ctor::Variant(*index), fields.iter().map(simplify).collect()),
        K::Some(inner) => P::C(Ctor::Some, vec![simplify(inner)]),
        K::None => P::C(Ctor::None, Vec::new()),
        K::Ok(inner) => P::C(Ctor::Ok, vec![simplify(inner)]),
        K::Err(inner) => P::C(Ctor::Err, vec![simplify(inner)]),
        K::Tuple(items) => P::C(Ctor::Tuple, items.iter().map(simplify).collect()),
        K::Or(alternatives) => P::Or(alternatives.iter().map(simplify).collect()),
        // A trait has no end to its implementations, so only a wildcard covers it.
        K::DynType { ty, .. } => P::C(Ctor::Literal(format!("type {ty:?}")), Vec::new()),
    }
}

/// Rows of the matrix specialised to constructor `ctor` with `arity` fields.
fn specialize(row: &[P], ctor: &Ctor, arity: usize) -> Vec<Vec<P>> {
    match &row[0] {
        P::Wild => vec![std::iter::repeat_n(P::Wild, arity).chain(row[1..].iter().cloned()).collect()],
        P::C(c, args) if c == ctor => vec![args.iter().cloned().chain(row[1..].iter().cloned()).collect()],
        P::C(..) => Vec::new(),
        P::Or(alternatives) => alternatives
            .iter()
            .flat_map(|a| {
                let mut expanded = vec![a.clone()];
                expanded.extend(row[1..].iter().cloned());
                specialize(&expanded, ctor, arity)
            })
            .collect(),
    }
}

/// Rows whose first column matches anything, without that column.
fn default_rows(row: &[P]) -> Vec<Vec<P>> {
    match &row[0] {
        P::Wild => vec![row[1..].to_vec()],
        P::C(..) => Vec::new(),
        P::Or(alternatives) => alternatives
            .iter()
            .flat_map(|a| {
                let mut expanded = vec![a.clone()];
                expanded.extend(row[1..].iter().cloned());
                default_rows(&expanded)
            })
            .collect(),
    }
}

fn head_ctors(row: &[P], out: &mut Vec<Ctor>) {
    match &row[0] {
        P::C(c, _) => {
            if !out.contains(c) {
                out.push(c.clone());
            }
        }
        P::Or(alternatives) => alternatives.iter().for_each(|a| head_ctors(std::slice::from_ref(a), out)),
        P::Wild => {}
    }
}

impl Checker<'_, '_> {
    /// Every constructor of a type with finitely many, and their field types; `None` for types
    /// such as `int` and `str` that only a wildcard can cover.
    fn ctors(&self, ty: Ty) -> Option<Vec<(Ctor, Vec<Ty>)>> {
        Some(match self.types.kind(ty) {
            TyKind::Enum(id) => self.enums[id.0]
                .variants
                .iter()
                .enumerate()
                .map(|(i, v)| (Ctor::Variant(i), v.fields.iter().map(|f| f.ty).collect()))
                .collect(),
            TyKind::Bool => vec![(Ctor::Bool(true), Vec::new()), (Ctor::Bool(false), Vec::new())],
            TyKind::Option(inner) => vec![(Ctor::Some, vec![*inner]), (Ctor::None, Vec::new())],
            TyKind::Result(ok, err) => vec![(Ctor::Ok, vec![*ok]), (Ctor::Err, vec![*err])],
            TyKind::Tuple(items) => vec![(Ctor::Tuple, items.clone())],
            _ => return None,
        })
    }

    fn ctor_fields(&self, ctor: &Ctor, ty: Ty) -> Vec<Ty> {
        self.ctors(ty).and_then(|all| all.into_iter().find(|(c, _)| c == ctor).map(|(_, f)| f)).unwrap_or_default()
    }

    fn useful(&self, rows: &[Vec<P>], q: &[P], tys: &[Ty]) -> bool {
        if q.is_empty() {
            return rows.is_empty();
        }
        match &q[0] {
            P::Or(alternatives) => alternatives.iter().any(|a| {
                let mut expanded = vec![a.clone()];
                expanded.extend(q[1..].iter().cloned());
                self.useful(rows, &expanded, tys)
            }),
            P::C(ctor, args) => {
                let fields = self.ctor_fields(ctor, tys[0]);
                let fields = if fields.len() == args.len() { fields } else { vec![Ty::ERROR; args.len()] };
                let new_rows: Vec<Vec<P>> = rows.iter().flat_map(|r| specialize(r, ctor, args.len())).collect();
                let new_q: Vec<P> = args.iter().cloned().chain(q[1..].iter().cloned()).collect();
                let new_tys: Vec<Ty> = fields.into_iter().chain(tys[1..].iter().copied()).collect();
                self.useful(&new_rows, &new_q, &new_tys)
            }
            P::Wild => {
                let mut used = Vec::new();
                rows.iter().for_each(|r| head_ctors(r, &mut used));
                match self.ctors(tys[0]) {
                    Some(all) if !all.is_empty() && all.iter().all(|(c, _)| used.contains(c)) => {
                        all.iter().any(|(ctor, fields)| {
                            let new_rows: Vec<Vec<P>> =
                                rows.iter().flat_map(|r| specialize(r, ctor, fields.len())).collect();
                            let new_q: Vec<P> =
                                std::iter::repeat_n(P::Wild, fields.len()).chain(q[1..].iter().cloned()).collect();
                            let new_tys: Vec<Ty> = fields.iter().copied().chain(tys[1..].iter().copied()).collect();
                            self.useful(&new_rows, &new_q, &new_tys)
                        })
                    }
                    _ => {
                        let new_rows: Vec<Vec<P>> = rows.iter().flat_map(|r| default_rows(r)).collect();
                        self.useful(&new_rows, &q[1..], &tys[1..])
                    }
                }
            }
        }
    }

    fn ctor_name(&self, ctor: &Ctor, ty: Ty) -> String {
        match (ctor, self.types.kind(ty)) {
            (Ctor::Variant(i), TyKind::Enum(id)) => {
                let variant = &self.enums[id.0].variants[*i];
                if variant.fields.is_empty() { variant.name.clone() } else { format!("{}(...)", variant.name) }
            }
            (Ctor::Bool(b), _) => if *b { "True" } else { "False" }.to_string(),
            (Ctor::Some, _) => "Some(...)".to_string(),
            (Ctor::None, _) => "None".to_string(),
            (Ctor::Ok, _) => "Ok(...)".to_string(),
            (Ctor::Err, _) => "Err(...)".to_string(),
            _ => "some values".to_string(),
        }
    }

    pub(super) fn match_stmt(
        &mut self,
        cx: &mut FnCx,
        subject: &ast::Expr,
        cases: &[ast::Case],
        span: Span,
    ) -> (Vec<hir::Stmt>, bool) {
        let subject = self.expr(cx, subject, None);
        let ty = subject.ty;
        self.check_match_subject(&subject);
        let before = cx.assigned.clone();
        let mut lowered = Vec::new();
        let mut states = Vec::new();
        let mut pattern_errors = false;
        for case in cases {
            cx.assigned = before.clone();
            let mut bindings = Vec::new();
            let pattern_start = self.diags.error_count();
            let pattern = self.pattern(cx, &case.pattern, ty, &mut bindings);
            pattern_errors |= self.diags.error_count() > pattern_start;
            for local in bindings {
                cx.set_assigned(local);
            }
            let guard = case.guard.as_ref().map(|g| self.condition(cx, g));
            let (body, diverges) = self.block(cx, &case.body);
            states.push((std::mem::take(&mut cx.assigned), diverges));
            lowered.push(hir::MatchCase { pattern, guard, body });
        }

        // After a bad pattern, coverage cannot be judged; that error is the one to fix first.
        let exhaustive = self.types.contains_error(ty) || pattern_errors || self.check_cases(&lowered, cases, ty, span);
        let live: Vec<&Vec<bool>> = states.iter().filter(|(_, d)| !d).map(|(s, _)| s).collect();
        cx.assigned = match live.split_first() {
            None => before,
            Some((first, rest)) => {
                let mut merged = (*first).clone();
                for state in rest {
                    merged = super::stmt::merge(&merged, state);
                }
                if exhaustive { merged } else { super::stmt::merge(&merged, &before) }
            }
        };
        // A `match` already reported as not exhaustive still counts as diverging when its cases
        // do, so the missing case is the only error, not also a missing `return`.
        let diverges = states.iter().all(|(_, d)| *d);
        (vec![hir::Stmt::Match { subject, cases: lowered }], diverges)
    }

    fn check_match_subject(&mut self, subject: &hir::Expr) {
        if matches!(self.types.kind(subject.ty), TyKind::SelfType(_)) {
            let shown = self.display(subject.ty);
            self.error(
                Diagnostic::error(
                    codes::BAD_PATTERN,
                    format!("cannot `match` on a `{shown}` trait object yet"),
                    subject.span,
                    "a trait object",
                )
                .with_help("call the trait's methods instead, or match on a concrete enum"),
            );
        }
    }

    /// `label = match status:` with a value on each `case` line (docs/11, milestone 1.7). It
    /// becomes a block holding a `match` statement whose cases store their value in a hidden
    /// local, which the block then yields. The cases must be exhaustive. Their values take the
    /// type the context expects, or else the first one's, as the two branches of `a if c else b`
    /// do.
    pub(super) fn match_expr(
        &mut self,
        cx: &mut FnCx,
        subject: &ast::Expr,
        cases: &[ast::Case],
        span: Span,
        expected: Option<Ty>,
    ) -> hir::Expr {
        let subject = self.expr(cx, subject, None);
        let ty = subject.ty;
        self.check_match_subject(&subject);
        let before = cx.assigned.clone();
        let mut lowered = Vec::new();
        let mut values: Vec<hir::Expr> = Vec::new();
        let mut states = Vec::new();
        let mut pattern_errors = false;
        let mut first_type = None;
        for case in cases {
            cx.assigned = before.clone();
            let mut bindings = Vec::new();
            let pattern_start = self.diags.error_count();
            let pattern = self.pattern(cx, &case.pattern, ty, &mut bindings);
            pattern_errors |= self.diags.error_count() > pattern_start;
            for local in bindings {
                cx.set_assigned(local);
            }
            let guard = case.guard.as_ref().map(|g| self.condition(cx, g));
            let ast::StmtKind::Expr(body) = &case.body[0].kind else { unreachable!("the parser gives one value") };
            let value = self.expr(cx, body, expected.or(first_type));
            if first_type.is_none() && value.ty != Ty::NONE && !is_panic(&value) {
                first_type = Some(value.ty);
            }
            states.push(std::mem::take(&mut cx.assigned));
            lowered.push(hir::MatchCase { pattern, guard, body: Vec::new() });
            values.push(value);
        }
        let exhaustive = self.types.contains_error(ty) || pattern_errors || self.check_cases(&lowered, cases, ty, span);
        cx.assigned = match states.split_first() {
            None => before.clone(),
            Some((first, rest)) => {
                let merged = rest.iter().fold(first.clone(), |merged, state| super::stmt::merge(&merged, state));
                if exhaustive { merged } else { super::stmt::merge(&merged, &before) }
            }
        };
        if values.iter().any(|v| v.ty == Ty::ERROR) {
            return error_expr(span);
        }

        // The type of the whole: a case that panics has no value, and one that is `None` makes
        // the others optional.
        let live: Vec<usize> = (0..values.len()).filter(|&i| !is_panic(&values[i])).collect();
        let result_ty = match first_type {
            None => expected.filter(|_| live.is_empty()).unwrap_or(Ty::NONE),
            Some(first) => {
                let others_fit = live.iter().all(|&i| self.types.fits(values[i].ty, first));
                if others_fit {
                    first
                } else if let Some(e) = expected.filter(|e| live.iter().all(|&i| self.coercible(values[i].ty, *e))) {
                    e
                } else if live.iter().all(|&i| values[i].ty == Ty::NONE || self.types.fits(values[i].ty, first)) {
                    self.types.option(first)
                } else {
                    let bad = *live.iter().find(|&&i| !self.types.fits(values[i].ty, first)).unwrap();
                    let (a, b) = (self.display(first), self.display(values[bad].ty));
                    let first_span = live.iter().map(|&i| &values[i]).find(|v| v.ty == first).unwrap().span;
                    self.error(
                        Diagnostic::error(
                            codes::TYPE_MISMATCH,
                            format!("the cases have different types, `{a}` and `{b}`"),
                            values[bad].span,
                            format!("this is `{b}`"),
                        )
                        .with_label(first_span, format!("this is `{a}`")),
                    );
                    return error_expr(span);
                }
            }
        };

        let temp = (result_ty != Ty::NONE).then(|| cx.temp(result_ty, span));
        for (case, value) in lowered.iter_mut().zip(values) {
            case.body = match temp {
                Some(local) if !is_panic(&value) => {
                    let value = self.coerce(value, result_ty, None);
                    vec![hir::Stmt::Assign { place: hir::Place::Local(local), value }]
                }
                _ => vec![hir::Stmt::Expr(value)],
            };
        }
        let result = match temp {
            Some(local) => hir::Expr { kind: hir::ExprKind::Local(local), ty: result_ty, span },
            None => hir::Expr { kind: hir::ExprKind::None, ty: Ty::NONE, span },
        };
        hir::Expr {
            kind: hir::ExprKind::Block {
                stmts: vec![hir::Stmt::Match { subject, cases: lowered }],
                value: Box::new(result),
            },
            ty: result_ty,
            span,
        }
    }

    /// Report a non-exhaustive `match` and unreachable cases. Returns whether it is exhaustive.
    fn check_cases(&mut self, cases: &[hir::MatchCase], ast_cases: &[ast::Case], ty: Ty, span: Span) -> bool {
        let mut rows: Vec<Vec<P>> = Vec::new();
        for (case, ast_case) in cases.iter().zip(ast_cases) {
            let row = vec![simplify(&case.pattern)];
            if !self.useful(&rows, &row, &[ty]) {
                self.diags.push(
                    Diagnostic::warning(
                        codes::UNREACHABLE_CASE,
                        "this case can never match",
                        ast_case.pattern.span,
                        "covered by the cases above",
                    )
                    .with_help("remove it, or move it before the case that covers it"),
                );
            }
            // A guarded case may not match, so it does not count towards covering the type.
            if case.guard.is_none() {
                rows.push(row);
            }
        }
        if !self.useful(&rows, &[P::Wild], &[ty]) {
            return true;
        }
        let missing: Vec<String> = match self.ctors(ty) {
            Some(all) => all
                .iter()
                .filter(|(ctor, fields)| {
                    let specialized: Vec<Vec<P>> =
                        rows.iter().flat_map(|r| specialize(r, ctor, fields.len())).collect();
                    self.useful(&specialized, &vec![P::Wild; fields.len()], fields)
                })
                .map(|(ctor, _)| self.ctor_name(ctor, ty))
                .collect(),
            None => Vec::new(),
        };
        let label = if missing.is_empty() || missing.iter().any(|m| m == "some values") {
            "some values are not covered".to_string()
        } else {
            format!("not covered: {}", missing.join(", "))
        };
        let help = match self.types.kind(ty) {
            TyKind::Dyn(trait_id) => format!(
                "any type can implement `{}`, so end the `match` with `case _:` for the rest",
                self.traits[trait_id.0].name
            ),
            _ => "add the missing cases, or `case _:` to cover everything else".to_string(),
        };
        self.error(
            Diagnostic::error(codes::NON_EXHAUSTIVE, "this `match` does not cover every case", span, label)
                .with_help(help),
        );
        false
    }

    /// Check a pattern against the type it matches, declaring the names it binds.
    fn pattern(&mut self, cx: &mut FnCx, pattern: &ast::Pattern, ty: Ty, bindings: &mut Vec<LocalId>) -> hir::Pattern {
        let wild = hir::Pattern { kind: K::Wild, ty };
        let kind = match &pattern.kind {
            ast::PatternKind::Wild => K::Wild,
            ast::PatternKind::Name(name) => {
                if let TyKind::Enum(id) = *self.types.kind(ty)
                    && let Some(index) = self.enums[id.0].variants.iter().position(|v| v.name == name.name)
                {
                    if !self.enums[id.0].variants[index].fields.is_empty() {
                        let count = self.enums[id.0].variants[index].fields.len();
                        self.error(
                            Diagnostic::error(
                                codes::BAD_PATTERN,
                                format!("variant `{}` has {}", name.name, plural(count, "field")),
                                name.span,
                                "fields not matched",
                            )
                            .with_help(format!(
                                "write `{}(...)` with a pattern for each field, using `_` for ones you do not need",
                                name.name
                            )),
                        );
                        return wild;
                    }
                    K::Variant { index, fields: Vec::new() }
                } else if let Some(owners) =
                    self.variant_names.get(&name.name).filter(|_| !matches!(self.types.kind(ty), TyKind::Error))
                {
                    let owner = self.enums[owners[0].0.0].name.clone();
                    let shown = self.display(ty);
                    self.error(Diagnostic::error(
                        codes::BAD_PATTERN,
                        format!("`{}` is a variant of `{owner}`, but the value being matched is `{shown}`", name.name),
                        name.span,
                        "wrong enum",
                    ));
                    return wild;
                } else {
                    match self.bind(cx, name, ty) {
                        Some(local) => {
                            bindings.push(local);
                            K::Bind(local)
                        }
                        None => K::Wild,
                    }
                }
            }
            ast::PatternKind::Literal(expr) => {
                if !matches!(
                    self.types.kind(ty),
                    TyKind::Int | TyKind::Float | TyKind::Str | TyKind::Bool | TyKind::Error
                ) {
                    let shown = self.display(ty);
                    self.error(Diagnostic::error(
                        codes::BAD_PATTERN,
                        format!("a literal cannot match a value of type `{shown}`"),
                        pattern.span,
                        "literal pattern",
                    ));
                    return wild;
                }
                K::Literal(self.expect(cx, expr, ty, None))
            }
            ast::PatternKind::Variant { path, args } => {
                return self.variant_pattern(cx, path, args.as_deref(), ty, pattern.span, bindings);
            }
            ast::PatternKind::Tuple(items) => match self.types.kind(ty).clone() {
                TyKind::Tuple(types) if types.len() == items.len() => {
                    K::Tuple(items.iter().zip(types).map(|(p, t)| self.pattern(cx, p, t, bindings)).collect())
                }
                TyKind::Error => K::Wild,
                _ => {
                    let shown = self.display(ty);
                    self.error(Diagnostic::error(
                        codes::BAD_PATTERN,
                        format!(
                            "a tuple pattern with {} cannot match a value of type `{shown}`",
                            plural(items.len(), "element")
                        ),
                        pattern.span,
                        "does not fit",
                    ));
                    return wild;
                }
            },
            ast::PatternKind::Or(alternatives) => {
                let mut first_names: Option<BTreeSet<String>> = None;
                let mut lowered = Vec::new();
                for alternative in alternatives {
                    let mut alt_bindings = Vec::new();
                    lowered.push(self.pattern(cx, alternative, ty, &mut alt_bindings));
                    let names: BTreeSet<String> = alt_bindings.iter().map(|l| cx.locals[l.0].name.clone()).collect();
                    match &first_names {
                        None => first_names = Some(names),
                        Some(first) if *first != names => {
                            self.error(
                                Diagnostic::error(
                                    codes::BAD_PATTERN,
                                    "each alternative must bind the same names",
                                    alternative.span,
                                    "binds different names",
                                )
                                .with_help("every `|` alternative must provide every name the case body uses"),
                            );
                        }
                        Some(_) => {}
                    }
                    bindings.extend(alt_bindings);
                }
                K::Or(lowered)
            }
        };
        hir::Pattern { kind, ty }
    }

    /// `case Circle(c):` or `case Circle():` on a trait object: the value is a `Circle`, bound to
    /// `c` as one. The type must be a struct or enum that implements the trait.
    #[allow(clippy::too_many_arguments)]
    fn dyn_type_pattern(
        &mut self,
        cx: &mut FnCx,
        path: &[ast::Ident],
        args: Option<&[ast::Pattern]>,
        trait_id: crate::types::TraitId,
        ty: Ty,
        span: Span,
        bindings: &mut Vec<LocalId>,
    ) -> hir::Pattern {
        let wild = hir::Pattern { kind: K::Wild, ty };
        let shown = self.display(ty);
        let trait_name = self.traits[trait_id.0].name.clone();
        let fail = |this: &mut Self, message: String, label: &str, help: &str| {
            this.error(Diagnostic::error(codes::BAD_PATTERN, message, span, label.to_string()).with_help(help));
        };
        let [name] = path else {
            fail(
                self,
                format!("a pattern for a `{shown}` names a type"),
                "not a type name",
                "write the type's name, as in `case Circle(c):`",
            );
            return wild;
        };
        let concrete = match self.globals.get(&name.name).map(|(g, _)| *g) {
            Some(Global::Struct(id)) => self.structs[id.0].ty,
            Some(Global::Enum(id)) => self.enums[id.0].ty,
            Some(Global::GenericStruct(_) | Global::GenericEnum(_)) => {
                fail(
                    self,
                    format!(
                        "`{}` is generic, and a pattern on a `{shown}` cannot name its type arguments yet",
                        name.name
                    ),
                    "generic type",
                    "match a non-generic type, or call the trait's methods",
                );
                return wild;
            }
            _ => {
                fail(
                    self,
                    format!("`{}` is not a struct or enum, which a `{shown}` can be", name.name),
                    "not a type that implements the trait",
                    "write the name of a type that implements the trait, as in `case Circle(c):`",
                );
                return wild;
            }
        };
        if !self.implements(concrete, trait_id) {
            let concrete_shown = self.display(concrete);
            fail(
                self,
                format!("`{concrete_shown}` does not implement `{trait_name}`, so a `{shown}` is never one"),
                "this case can never match",
                &format!("implement `{trait_name}` for `{concrete_shown}`, or remove the case"),
            );
            return wild;
        }
        let inner = match args {
            None | Some([]) => hir::Pattern { kind: K::Wild, ty: concrete },
            Some([only]) if matches!(only.kind, ast::PatternKind::Name(_) | ast::PatternKind::Wild) => {
                self.pattern(cx, only, concrete, bindings)
            }
            Some(_) => {
                fail(
                    self,
                    format!("a pattern for a `{shown}` takes one name for the whole value, or none"),
                    "too much",
                    "write `case Circle(c):` to bind the value, or `case Circle():` to only test its type",
                );
                return wild;
            }
        };
        hir::Pattern { kind: K::DynType { ty: concrete, inner: Box::new(inner) }, ty }
    }

    fn variant_pattern(
        &mut self,
        cx: &mut FnCx,
        path: &[ast::Ident],
        args: Option<&[ast::Pattern]>,
        ty: Ty,
        span: Span,
        bindings: &mut Vec<LocalId>,
    ) -> hir::Pattern {
        let wild = hir::Pattern { kind: K::Wild, ty };
        if matches!(self.types.kind(ty), TyKind::Error) {
            for arg in args.unwrap_or_default() {
                self.pattern(cx, arg, Ty::ERROR, bindings);
            }
            return wild;
        }
        if let TyKind::Dyn(trait_id) = *self.types.kind(ty) {
            return self.dyn_type_pattern(cx, path, args, trait_id, ty, span, bindings);
        }
        let shown = self.display(ty);
        let mismatch = |this: &mut Self, what: &str| {
            this.error(Diagnostic::error(
                codes::BAD_PATTERN,
                format!("`{what}` cannot match a value of type `{shown}`"),
                span,
                "does not fit",
            ));
        };
        // The built-in enums.
        if path.len() == 1 && matches!(path[0].name.as_str(), "None" | "Some" | "Ok" | "Err") {
            let name = path[0].name.as_str();
            let inner = match (name, self.types.kind(ty).clone()) {
                ("None", TyKind::Option(_)) => {
                    if args.is_some() {
                        mismatch(self, "None(...)");
                        return wild;
                    }
                    return hir::Pattern { kind: K::None, ty };
                }
                ("Some", TyKind::Option(inner))
                | ("Ok", TyKind::Result(inner, _))
                | ("Err", TyKind::Result(_, inner)) => inner,
                _ => {
                    mismatch(self, name);
                    return wild;
                }
            };
            let Some([arg]) = args else {
                self.error(Diagnostic::error(
                    codes::BAD_PATTERN,
                    format!("`{name}` takes one pattern: `{name}(x)`"),
                    span,
                    "expected one pattern",
                ));
                return wild;
            };
            let inner_pattern = Box::new(self.pattern(cx, arg, inner, bindings));
            let kind = match name {
                "Some" => K::Some(inner_pattern),
                "Ok" => K::Ok(inner_pattern),
                _ => K::Err(inner_pattern),
            };
            return hir::Pattern { kind, ty };
        }

        let TyKind::Enum(enum_id) = *self.types.kind(ty) else {
            let written = path.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(".");
            mismatch(self, &written);
            return wild;
        };
        let variant_name = match path {
            [variant] => variant,
            [prefix @ .., variant] => {
                // `Enum.Variant`, or `module.Enum.Variant`.
                let type_name = prefix.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(".");
                let type_span = prefix[0].span.to(prefix[prefix.len() - 1].span);
                let same = match self.type_global(&type_name, type_span) {
                    Some(Global::Enum(id)) => id == enum_id,
                    Some(Global::GenericEnum(template)) => {
                        self.instance_of.get(&ty).is_some_and(|(t, _)| *t == template)
                    }
                    Some(Global::Missing) => return wild,
                    _ => false,
                };
                if !same {
                    self.error(Diagnostic::error(
                        codes::BAD_PATTERN,
                        format!("the value being matched is `{shown}`, not `{type_name}`"),
                        type_span,
                        "wrong enum",
                    ));
                    return wild;
                }
                variant
            }
            [] => unreachable!("a pattern path has at least one part"),
        };
        let Some(index) = self.enums[enum_id.0].variants.iter().position(|v| v.name == variant_name.name) else {
            let candidates: Vec<String> = self.enums[enum_id.0].variants.iter().map(|v| v.name.clone()).collect();
            let mut diagnostic = Diagnostic::error(
                codes::BAD_PATTERN,
                format!("`{shown}` has no variant `{}`", variant_name.name),
                variant_name.span,
                "no such variant",
            );
            if let Some(s) = super::closest(&variant_name.name, candidates.iter().map(String::as_str)) {
                diagnostic = diagnostic.with_help(format!("did you mean `{s}`?"));
            }
            self.error(diagnostic);
            return wild;
        };
        let field_types: Vec<Ty> = self.enums[enum_id.0].variants[index].fields.iter().map(|f| f.ty).collect();
        let args = args.unwrap_or_default();
        if args.len() != field_types.len() {
            self.error(
                Diagnostic::error(
                    codes::BAD_PATTERN,
                    format!(
                        "variant `{}` has {}, but the pattern has {}",
                        variant_name.name,
                        plural(field_types.len(), "field"),
                        args.len()
                    ),
                    span,
                    "wrong number of fields",
                )
                .with_help("give one pattern per field, using `_` for fields you do not need"),
            );
            return wild;
        }
        let fields = args.iter().zip(field_types).map(|(p, t)| self.pattern(cx, p, t, bindings)).collect();
        hir::Pattern { kind: K::Variant { index, fields }, ty }
    }

    /// Declare (or reuse) the local a pattern binds.
    fn bind(&mut self, cx: &mut FnCx, name: &ast::Ident, ty: Ty) -> Option<LocalId> {
        if !cx.scope.contains_key(&name.name)
            && let Some(kind) = self.local_conflict(&name.name)
        {
            self.error(Diagnostic::error(
                codes::BAD_PATTERN,
                format!("cannot bind the name `{}`; it is {kind}", name.name),
                name.span,
                "already defined",
            ));
            return None;
        }
        match cx.scope.get(&name.name) {
            Some(&local) => {
                let declared = cx.locals[local.0].ty;
                if !self.types.fits(ty, declared) {
                    let (declared_shown, found) = (self.display(declared), self.display(ty));
                    let decl_span = cx.info[local.0].decl_span;
                    self.error(
                        Diagnostic::error(
                            codes::TYPE_MISMATCH,
                            format!("`{}` already has type `{declared_shown}`", name.name),
                            name.span,
                            format!("the pattern gives `{found}`"),
                        )
                        .with_label(decl_span, format!("`{}` was declared as `{declared_shown}` here", name.name)),
                    );
                }
                Some(local)
            }
            None => Some(cx.add_local(&name.name, ty, name.span, false)),
        }
    }
}

/// Whether an expression is a call to `panic`, which never gives a value.
fn is_panic(expr: &hir::Expr) -> bool {
    matches!(expr.kind, hir::ExprKind::Builtin { op: hir::Builtin::Panic, .. })
}
