//! Tasks, the synchronised types, and the sharing rules that prevent data races
//! (docs/04-runtime-and-memory.md, "Concurrency" and "Data-race prevention").
//!
//! `Shareable` is computed here for every type: immutable values, `@frozen` structs of shareable
//! fields, enums and tuples of shareable parts, and `Mutex`, `Atomic`, `Channel` and `Task`. It is
//! enforced where values cross between tasks: the arguments of `spawn`, a channel's element type
//! and the values sent on it, and a mutex's contents. The name a `with m.lock() as name:` block
//! binds may be used inside the block but may not escape it: it cannot be stored, returned, sent,
//! captured, or used after the block.

use super::{Checker, FnCx, error_expr, plural};
use crate::hir::{self, Builtin as Op, ExprKind as K, LocalId};
use crate::types::{Ty, TyKind};
use lang_diag::{Diagnostic, Span, codes};
use lang_syntax::ast;

fn mk(kind: hir::ExprKind, ty: Ty, span: Span) -> hir::Expr {
    hir::Expr { kind, ty, span }
}

/// The synchronised types' names, which work like built-in generic types.
pub(super) const SYNC_TYPES: &[&str] = &["Task", "Channel", "Mutex", "Atomic"];

impl Checker<'_, '_> {
    // ---- the Shareable property ---------------------------------------------------------

    pub(super) fn shareable(&self, ty: Ty) -> bool {
        self.why_not_shareable(ty, &mut Vec::new()).is_none()
    }

    /// Why values of `ty` cannot go to another task, and how to fix it; `None` if they can.
    pub(super) fn why_not_shareable(&self, ty: Ty, visiting: &mut Vec<Ty>) -> Option<(String, String)> {
        let shown = self.display(ty);
        let part = |this: &Self, inner: Ty, visiting: &mut Vec<Ty>| -> Option<(String, String)> {
            let (why, help) = this.why_not_shareable(inner, visiting)?;
            Some((format!("it contains a `{}`: {why}", this.display(inner)), help))
        };
        match self.types.kind(ty).clone() {
            TyKind::Error | TyKind::None | TyKind::Bool | TyKind::Int | TyKind::Float | TyKind::Str => None,
            TyKind::Sized(_) | TyKind::Atomic | TyKind::Mutex(_) | TyKind::Channel(_) | TyKind::Task(_) => None,
            TyKind::Guard(_) => Some((
                "a lock guard belongs to the `with` block holding the lock".to_string(),
                "use the value inside the block".to_string(),
            )),
            TyKind::FrozenList(t) | TyKind::Option(t) => part(self, t, visiting),
            TyKind::FrozenDict(k, v) | TyKind::Result(k, v) => {
                part(self, k, visiting).or_else(|| part(self, v, visiting))
            }
            TyKind::Tuple(items) => items.iter().find_map(|t| part(self, *t, visiting)),
            TyKind::List(elem) => Some((
                format!("a `{shown}` can be changed"),
                format!(
                    "share an immutable `tuple[{}, ...]` made with `tuple(items)`, or wrap the list in `Mutex(...)`",
                    self.display(elem)
                ),
            )),
            TyKind::Dict(_, Ty::NONE) => Some((
                format!("a `{shown}` can be changed"),
                "wrap the set in `Mutex(...)`; there is no immutable set yet".to_string(),
            )),
            TyKind::Dict(k, v) => Some((
                format!("a `{shown}` can be changed"),
                format!(
                    "share an immutable `frozendict[{}, {}]` made with `frozendict(d)`, or wrap the dict in `Mutex(...)`",
                    self.display(k),
                    self.display(v)
                ),
            )),
            TyKind::Struct(id) => {
                if visiting.contains(&ty) {
                    return None;
                }
                let info = &self.structs[id.0];
                if !info.frozen {
                    return Some((
                        format!("`{shown}` has fields that can be changed"),
                        format!("mark `{shown}` `@frozen`, or wrap the value in `Mutex(...)`"),
                    ));
                }
                visiting.push(ty);
                let fields: Vec<(String, Ty)> = info.fields.iter().map(|f| (f.name.clone(), f.ty)).collect();
                let result = fields.into_iter().find_map(|(name, field_ty)| {
                    let (why, help) = self.why_not_shareable(field_ty, visiting)?;
                    Some((format!("its field `{name}` is a `{}`: {why}", self.display(field_ty)), help))
                });
                visiting.pop();
                result
            }
            TyKind::Enum(id) => {
                if visiting.contains(&ty) {
                    return None;
                }
                visiting.push(ty);
                let fields: Vec<(String, Ty)> = self.enums[id.0]
                    .variants
                    .iter()
                    .flat_map(|v| v.fields.iter().map(move |f| (format!("{}.{}", v.name, f.name), f.ty)))
                    .collect();
                let result = fields.into_iter().find_map(|(name, field_ty)| {
                    let (why, help) = self.why_not_shareable(field_ty, visiting)?;
                    Some((format!("its field `{name}` is a `{}`: {why}", self.display(field_ty)), help))
                });
                visiting.pop();
                result
            }
            TyKind::Dyn(t) | TyKind::SelfType(t) => {
                if self.trait_closure(t).contains(&self.builtin().shareable) {
                    return None;
                }
                let name = self.traits[t.0].name.clone();
                Some((
                    format!("the trait `{name}` does not require its implementations to be shareable"),
                    format!("declare it as `trait {name}(Shareable):`"),
                ))
            }
            TyKind::Param(_) => {
                if self.implements(ty, self.builtin().shareable) {
                    return None;
                }
                Some((
                    format!("the type parameter `{shown}` might be a type that can be changed"),
                    format!("add the bound `{shown}: Shareable`"),
                ))
            }
            TyKind::Func(..) => Some((
                "a function value might capture variables that can be changed".to_string(),
                "pass a named function, or the data the function needs".to_string(),
            )),
        }
    }

    /// Report if `value` cannot be handed to another task; `what` says how it would be.
    pub(super) fn require_shareable(&mut self, cx: &FnCx, value: &hir::Expr, what: &str) -> bool {
        if self.types.contains_error(value.ty) {
            return true;
        }
        let Some((why, help)) = self.value_not_shareable(cx, value) else { return true };
        let shown = self.display(value.ty);
        self.error(
            Diagnostic::error(
                codes::NOT_SHAREABLE,
                format!("a value of type `{shown}` cannot be {what}"),
                value.span,
                why,
            )
            .with_help(help),
        );
        false
    }

    /// Why this value cannot go to another task, looking into what a function value carries.
    fn value_not_shareable(&self, cx: &FnCx, value: &hir::Expr) -> Option<(String, String)> {
        match (&value.kind, self.types.kind(value.ty)) {
            // A function the standard library makes from other functions, such as
            // `auth.protect(handler)`, carries only those functions: it is shareable if they are.
            (K::Call { func, args }, TyKind::Func(..)) if self.is_stdlib_function(*func) => args
                .iter()
                .filter(|arg| matches!(self.types.kind(arg.ty), TyKind::Func(..)))
                .find_map(|arg| self.value_not_shareable(cx, arg)),
            // `Some(f)` for an optional function.
            (K::Some(inner), _) => self.value_not_shareable(cx, inner),
            (K::None, _) => None,
            _ => self.plain_not_shareable(cx, value),
        }
    }

    /// Whether a function is declared in a standard library module.
    /// A call of a `jobs` function from the program: the functions it is given run in tasks of
    /// their own (a schedule's job, a queue's handler), so they must be shareable.
    pub(super) fn check_jobs_call(&mut self, cx: &FnCx, func: hir::FuncId, args: &[hir::Expr]) {
        if self.is_stdlib_module(self.current_module) {
            return;
        }
        let module = self.module_of(self.signatures[func.0].name_span);
        if self.module_names.get(module).map(String::as_str) != Some("jobs") {
            return;
        }
        for arg in args {
            if matches!(self.types.kind(arg.ty), TyKind::Func(..)) {
                self.require_shareable(cx, arg, "run in another task");
            }
        }
    }

    pub(super) fn is_stdlib_function(&self, func: hir::FuncId) -> bool {
        self.is_stdlib_module(self.module_of(self.signatures[func.0].name_span))
    }

    /// Whether a module is one of the standard library's (not the prelude).
    pub(super) fn is_stdlib_module(&self, module: usize) -> bool {
        module != 0 && self.stdlib_modules.get(module).copied().unwrap_or(false)
    }

    fn plain_not_shareable(&self, cx: &FnCx, value: &hir::Expr) -> Option<(String, String)> {
        match (&value.kind, self.types.kind(value.ty)) {
            // A function value is shareable if what it carries is.
            (K::FuncRef(_), TyKind::Func(..)) => None,
            (K::BoundMethod { receiver, .. }, TyKind::Func(..)) => {
                self.why_not_shareable(receiver.ty, &mut Vec::new()).map(|(why, help)| {
                    (format!("it is a method bound to a `{}`, and {why}", self.display(receiver.ty)), help)
                })
            }
            (K::Closure { captures, .. }, TyKind::Func(..)) => captures.iter().find_map(|local| {
                let (name, ty) = (cx.locals[local.0].name.clone(), cx.locals[local.0].ty);
                let (why, help) = self.why_not_shareable(ty, &mut Vec::new())?;
                Some((format!("it captures `{name}`, a `{}`: {why}", self.display(ty)), help))
            }),
            _ => self.why_not_shareable(value.ty, &mut Vec::new()),
        }
    }

    /// What a mutex may hold: a shareable value, or one mutable layer (a list, dict or ordinary
    /// struct) over shareable contents.
    fn mutex_content_problem(&self, ty: Ty) -> Option<(String, String)> {
        if self.shareable(ty) {
            return None;
        }
        let mut visiting = Vec::new();
        match self.types.kind(ty).clone() {
            TyKind::List(elem) => self.why_not_shareable(elem, &mut visiting),
            TyKind::Dict(k, v) => {
                self.why_not_shareable(k, &mut visiting).or_else(|| self.why_not_shareable(v, &mut visiting))
            }
            TyKind::Struct(id) => self.structs[id.0].fields.iter().find_map(|f| {
                let (why, help) = self.why_not_shareable(f.ty, &mut Vec::new())?;
                Some((format!("its field `{}` is a `{}`: {why}", f.name, self.display(f.ty)), help))
            }),
            _ => self.why_not_shareable(ty, &mut visiting),
        }
    }

    fn check_element_type(&mut self, container: &str, elem: Ty, span: Span) -> bool {
        let problem = if container == "Mutex" {
            self.mutex_content_problem(elem)
        } else {
            self.why_not_shareable(elem, &mut Vec::new())
        };
        let Some((why, help)) = problem else { return true };
        let shown = self.display(elem);
        let what = match container {
            "Mutex" => "protected by a `Mutex`",
            _ => "sent on a `Channel`",
        };
        self.error(
            Diagnostic::error(codes::NOT_SHAREABLE, format!("a value of type `{shown}` cannot be {what}"), span, why)
                .with_help(help),
        );
        false
    }

    // ---- types --------------------------------------------------------------------------

    /// `Task[T]`, `Channel[T]`, `Mutex[T]`, `Atomic`, `frozendict[K, V]` and `tuple[T, ...]`
    /// as type hints; `None` if `name` is not one of them.
    pub(super) fn special_type(&mut self, name: &str, args: &[ast::TypeExpr], span: Span) -> Option<Ty> {
        let ellipsis = |a: &ast::TypeExpr| matches!(&a.kind, ast::TypeExprKind::Named { name, .. } if name == "...");
        Some(match name {
            "tuple" if args.len() == 2 && ellipsis(&args[1]) => {
                let elem = self.resolve_value_type(&args[0]);
                self.types.frozen_list(elem)
            }
            "frozendict" if args.len() == 2 => {
                let key = self.resolve_value_type(&args[0]);
                let value = self.resolve_value_type(&args[1]);
                self.pending_hashable.push((key, args[0].span));
                self.types.frozen_dict(key, value)
            }
            "Atomic" if args.is_empty() || (args.len() == 1 && self.resolve_value_type(&args[0]) == Ty::INT) => {
                self.types.intern(TyKind::Atomic)
            }
            "Task" | "Channel" | "Mutex" if args.len() == 1 => {
                let elem = if name == "Task" { self.resolve_type(&args[0]) } else { self.resolve_value_type(&args[0]) };
                if elem == Ty::ERROR {
                    return Some(Ty::ERROR);
                }
                match name {
                    "Task" => self.types.intern(TyKind::Task(elem)),
                    _ if !self.check_element_type(name, elem, span) => Ty::ERROR,
                    "Channel" => self.types.intern(TyKind::Channel(elem)),
                    _ => self.types.intern(TyKind::Mutex(elem)),
                }
            }
            "frozendict" | "Atomic" | "Task" | "Channel" | "Mutex" => {
                let expected = match name {
                    "frozendict" => "`frozendict[K, V]`",
                    "Atomic" => "`Atomic`, which holds an `int`",
                    _ => "one type argument",
                };
                self.error(Diagnostic::error(
                    codes::UNEXPECTED_TYPE_ARGS,
                    format!("wrong type arguments for `{name}`"),
                    span,
                    format!("expected {expected}"),
                ));
                Ty::ERROR
            }
            _ => return None,
        })
    }

    // ---- spawn --------------------------------------------------------------------------

    /// `spawn f(args)`.
    pub(super) fn spawn_expr(&mut self, cx: &mut FnCx, call: &ast::Expr, span: Span) -> hir::Expr {
        let checked = self.expr(cx, call, None);
        if self.types.contains_error(checked.ty) {
            return error_expr(span);
        }
        let result_ty = checked.ty;
        let (func, callee, args) = match checked.kind {
            K::Call { func, args } => (Some(func), None, args),
            K::CallValue { callee, args } => (None, Some(callee), args),
            _ => {
                self.error(
                    Diagnostic::error(
                        codes::NOT_SUPPORTED,
                        "`spawn` can only start a call to a function or method",
                        call.span,
                        "not a function call",
                    )
                    .with_help("built-in functions and the methods of built-in types cannot be spawned; put the work in a function"),
                );
                return error_expr(span);
            }
        };
        // The standard library runs callbacks it was given in tasks of their own (the checks of
        // `server.health`); it checks them where they are given to it.
        let trusted = |this: &Self, value: &hir::Expr| {
            this.is_stdlib_module(this.current_module) && matches!(this.types.kind(value.ty), TyKind::Func(..))
        };
        if let Some(callee) = &callee {
            if !trusted(self, callee) {
                self.require_shareable(cx, callee, "called in another task");
            }
        }
        for arg in &args {
            if trusted(self, arg) {
                continue;
            }
            if self.require_shareable(cx, arg, "passed to another task") {
                self.check_escape(cx, arg, "passed to another task");
            }
        }
        let ty = self.types.intern(TyKind::Task(result_ty));
        mk(K::Spawn { func, callee, args }, ty, span)
    }

    // ---- constructors and methods -------------------------------------------------------

    /// `Channel[T](capacity)`, `Mutex(value)`, `Atomic(value)`; `type_args` are the written
    /// `[...]`, if any.
    pub(super) fn sync_constructor(
        &mut self,
        cx: &mut FnCx,
        name: &str,
        type_args: Option<Vec<Ty>>,
        args: &[ast::Arg],
        span: Span,
        expected: Option<Ty>,
    ) -> hir::Expr {
        if let Some(arg) = args.iter().find(|a| a.name.is_some()) {
            self.error(Diagnostic::error(
                codes::NOT_SUPPORTED,
                format!("`{name}` takes no keyword arguments"),
                arg.value.span,
                "remove the name",
            ));
            self.check_all_quietly(cx, args);
            return error_expr(span);
        }
        let expected_inner = expected.and_then(|t| match (name, self.types.kind(t)) {
            ("Channel", TyKind::Channel(inner)) | ("Mutex", TyKind::Mutex(inner)) => Some(*inner),
            _ => None,
        });
        let explicit = match type_args.as_deref() {
            Some([one]) => Some(*one),
            Some(_) => {
                self.error(Diagnostic::error(
                    codes::UNEXPECTED_TYPE_ARGS,
                    format!("`{name}` takes one type argument"),
                    span,
                    "wrong number of type arguments",
                ));
                self.check_all_quietly(cx, args);
                return error_expr(span);
            }
            None => None,
        };
        let max_args = 1;
        if args.len() > max_args || (name == "Mutex" && args.is_empty()) {
            let what = match name {
                "Mutex" => "one value: `Mutex(value)`",
                "Atomic" => "an optional starting `int`: `Atomic(0)`",
                _ => "an optional capacity: `Channel[int](10)`",
            };
            self.error(Diagnostic::error(
                codes::WRONG_ARG_COUNT,
                format!("`{name}` takes {what}"),
                span,
                format!("{} given", plural(args.len(), "argument")),
            ));
            self.check_all_quietly(cx, args);
            return error_expr(span);
        }
        let int = |value: i64| mk(K::Int(value), Ty::INT, span);
        match name {
            "Atomic" => {
                let start = match args.first() {
                    Some(arg) => self.expect(cx, &arg.value, Ty::INT, None),
                    None => int(0),
                };
                let ty = self.types.intern(TyKind::Atomic);
                mk(K::Builtin { op: Op::AtomicNew, args: vec![start] }, ty, span)
            }
            "Mutex" => {
                let value = match explicit.or(expected_inner) {
                    Some(inner) => self.expect(cx, &args[0].value, inner, None),
                    None => self.expr(cx, &args[0].value, None),
                };
                if self.types.contains_error(value.ty) || !self.check_element_type("Mutex", value.ty, value.span) {
                    return error_expr(span);
                }
                self.check_escape(cx, &value, "put in a `Mutex`");
                // A mutable value must not stay reachable outside the lock.
                if !self.shareable(value.ty)
                    && let Some(local) = Self::root_local(&value)
                {
                    let name = cx.locals[local.0].name.clone();
                    self.error(
                        Diagnostic::error(
                            codes::NOT_SHAREABLE,
                            "a value in a `Mutex` must only be reachable through the lock",
                            value.span,
                            format!("`{name}` still refers to it, so it could be changed without the lock"),
                        )
                        .with_help("create the value inside the call, as in `Mutex([])` or `Mutex(State(...))`"),
                    );
                    return error_expr(span);
                }
                let ty = self.types.intern(TyKind::Mutex(value.ty));
                mk(K::Builtin { op: Op::MutexNew, args: vec![value] }, ty, span)
            }
            _ => {
                let Some(elem) = explicit.or(expected_inner) else {
                    self.error(
                        Diagnostic::error(
                            codes::CANNOT_INFER,
                            "cannot tell what this channel carries",
                            span,
                            "element type unknown",
                        )
                        .with_help("write the type: `Channel[int](10)`"),
                    );
                    self.check_all_quietly(cx, args);
                    return error_expr(span);
                };
                if !self.check_element_type("Channel", elem, span) {
                    self.check_all_quietly(cx, args);
                    return error_expr(span);
                }
                let capacity = match args.first() {
                    Some(arg) => self.expect(cx, &arg.value, Ty::INT, None),
                    None => int(0),
                };
                let ty = self.types.intern(TyKind::Channel(elem));
                mk(K::Builtin { op: Op::ChannelNew, args: vec![capacity] }, ty, span)
            }
        }
    }

    /// `tuple(items)` and `frozendict(d)`: immutable copies.
    pub(super) fn freeze_call(&mut self, cx: &mut FnCx, name: &str, args: &[ast::Arg], span: Span) -> hir::Expr {
        let [arg] = args else {
            self.error(Diagnostic::error(
                codes::WRONG_ARG_COUNT,
                format!("`{name}` takes one {}", if name == "tuple" { "list" } else { "dict" }),
                span,
                "expected one argument",
            ));
            self.check_all_quietly(cx, args);
            return error_expr(span);
        };
        let value = self.expr(cx, &arg.value, None);
        let ty = match (name, self.types.kind(value.ty).clone()) {
            (_, TyKind::Error) => return error_expr(span),
            ("tuple", TyKind::List(elem)) => self.types.frozen_list(elem),
            ("tuple", TyKind::FrozenList(_)) | ("frozendict", TyKind::FrozenDict(..)) => return value,
            ("frozendict", TyKind::Dict(k, v)) if v != Ty::NONE => self.types.frozen_dict(k, v),
            _ => {
                let shown = self.display(value.ty);
                let want = if name == "tuple" { "a list" } else { "a dict" };
                let mut diagnostic = Diagnostic::error(
                    codes::TYPE_MISMATCH,
                    format!("`{name}` cannot take a `{shown}`"),
                    value.span,
                    format!("expected {want}"),
                );
                if self.types.set_elem(value.ty).is_some() {
                    diagnostic = diagnostic.with_help("there is no immutable set; share a set through a `Mutex`");
                }
                self.error(diagnostic);
                return error_expr(span);
            }
        };
        mk(K::Builtin { op: Op::Freeze, args: vec![value] }, ty, span)
    }

    /// The methods of `Task`, `Channel`, `Mutex` and `Atomic`.
    pub(super) fn sync_method(
        &mut self,
        cx: &mut FnCx,
        receiver: hir::Expr,
        name: &ast::Ident,
        args: &[ast::Arg],
        span: Span,
    ) -> hir::Expr {
        let ty = receiver.ty;
        let kind = self.types.kind(ty).clone();
        let method = name.name.as_str();
        // (operation, parameter types, result type)
        let signature: Option<(Op, Vec<Ty>, Ty)> = match (&kind, method) {
            (TyKind::Task(t), "join") => Some((Op::TaskJoin, vec![], *t)),
            (TyKind::Task(t), "join_timeout") => {
                let ret = if *t == Ty::NONE { Ty::BOOL } else { self.types.option(*t) };
                Some((Op::TaskJoinTimeout, vec![Ty::FLOAT], ret))
            }
            (TyKind::Task(_), "is_done") => Some((Op::TaskIsDone, vec![], Ty::BOOL)),
            // For the test runner: wait for the task, and say whether it panicked.
            (TyKind::Task(_), "__panicked") => Some((Op::TaskPanicked, vec![], Ty::BOOL)),
            (TyKind::Channel(t), "send") => Some((Op::ChannelSend, vec![*t], Ty::NONE)),
            (TyKind::Channel(t), "recv") => Some((Op::ChannelRecv, vec![], self.types.option(*t))),
            (TyKind::Channel(t), "try_recv") => Some((Op::ChannelTryRecv, vec![], self.types.option(*t))),
            (TyKind::Channel(_), "close") => Some((Op::ChannelClose, vec![], Ty::NONE)),
            (TyKind::Atomic, "get") => Some((Op::AtomicGet, vec![], Ty::INT)),
            (TyKind::Atomic, "set") => Some((Op::AtomicSet, vec![Ty::INT], Ty::NONE)),
            (TyKind::Atomic, "add") => Some((Op::AtomicAdd, vec![Ty::INT], Ty::INT)),
            (TyKind::Atomic, "increment" | "decrement") => Some((Op::AtomicAdd, vec![], Ty::INT)),
            (TyKind::Atomic, "compare_and_set") => Some((Op::AtomicCas, vec![Ty::INT, Ty::INT], Ty::BOOL)),
            (TyKind::Mutex(_), "lock") => {
                self.error(
                    Diagnostic::error(
                        codes::BAD_WITH,
                        "a mutex is locked with a `with` block",
                        span,
                        "the lock would never be released",
                    )
                    .with_help("write `with m.lock() as value:`; the lock is released when the block ends"),
                );
                self.check_all_quietly(cx, args);
                return error_expr(span);
            }
            _ => None,
        };
        let Some((op, params, ret)) = signature else {
            let methods = match kind {
                TyKind::Task(_) => "`join`, `join_timeout` and `is_done`",
                TyKind::Channel(_) => "`send`, `recv`, `try_recv` and `close`",
                TyKind::Mutex(_) => "`lock`, in `with m.lock() as value:`",
                _ => "`get`, `set`, `add`, `increment`, `decrement` and `compare_and_set`",
            };
            let shown = self.display(ty);
            self.error(
                Diagnostic::error(
                    codes::NO_SUCH_MEMBER,
                    format!("`{shown}` has no method `{method}`"),
                    name.span,
                    "no such method",
                )
                .with_help(format!("its methods are {methods}")),
            );
            self.check_all_quietly(cx, args);
            return error_expr(span);
        };
        if args.iter().any(|a| a.name.is_some()) || args.len() != params.len() {
            self.error(Diagnostic::error(
                codes::WRONG_ARG_COUNT,
                format!("`{method}` takes {}", plural(params.len(), "argument")),
                span,
                format!("expected {}", plural(params.len(), "positional argument")),
            ));
            self.check_all_quietly(cx, args);
            return error_expr(span);
        }
        let mut values = vec![receiver];
        for (arg, param) in args.iter().zip(&params) {
            let value = self.expect(cx, &arg.value, *param, None);
            if op == Op::ChannelSend && self.require_shareable(cx, &value, "sent to another task") {
                self.check_escape(cx, &value, "sent to another task");
            }
            values.push(value);
        }
        match method {
            "increment" => values.push(mk(K::Int(1), Ty::INT, span)),
            "decrement" => values.push(mk(K::Int(-1), Ty::INT, span)),
            _ => {}
        }
        mk(K::Builtin { op, args: values }, ret, span)
    }

    // ---- lock guards --------------------------------------------------------------------

    /// `with m.lock() as name:`, when `object` is a mutex. `None` if it is not, in which case
    /// nothing has been checked.
    pub(super) fn lock_block(
        &mut self,
        cx: &mut FnCx,
        mutex: hir::Expr,
        target: Option<&ast::Ident>,
        body: &[ast::Stmt],
        span: Span,
    ) -> (Vec<hir::Stmt>, bool) {
        let TyKind::Mutex(inner) = *self.types.kind(mutex.ty) else { unreachable!("checked by the caller") };
        let guard_ty = self.types.intern(TyKind::Guard(inner));
        let temp = cx.temp(guard_ty, span);
        let lock = mk(K::Builtin { op: Op::MutexLock, args: vec![mutex] }, guard_ty, span);
        let mut inner_stmts = Vec::new();
        let bound = match target {
            Some(name) => {
                let get =
                    mk(K::Builtin { op: Op::GuardGet, args: vec![mk(K::Local(temp), guard_ty, span)] }, inner, span);
                let local = match cx.scope.get(&name.name) {
                    Some(&local) if cx.locals[local.0].ty == inner => local,
                    Some(&local) => {
                        let declared = self.display(cx.locals[local.0].ty);
                        self.error(Diagnostic::error(
                            codes::TYPE_MISMATCH,
                            format!("`{}` already has type `{declared}`", name.name),
                            name.span,
                            format!("the mutex holds a `{}`", self.display(inner)),
                        ));
                        return (Vec::new(), false);
                    }
                    None => cx.add_local(&name.name, inner, name.span, false),
                };
                cx.set_assigned(local);
                cx.expired.retain(|l| *l != local);
                cx.guarded.push(local);
                inner_stmts.push(hir::Stmt::Assign { place: hir::Place::Local(local), value: get });
                Some(local)
            }
            None => None,
        };
        let (body, diverges) = self.block(cx, body);
        inner_stmts.extend(body);
        if let Some(local) = bound {
            cx.guarded.retain(|l| *l != local);
            cx.expired.push(local);
        }
        let stmts = vec![
            hir::Stmt::Assign { place: hir::Place::Local(temp), value: lock },
            hir::Stmt::With { object: temp, exit: hir::WithExit::Unlock { value: bound }, body: inner_stmts },
        ];
        (stmts, diverges)
    }

    /// The local an expression reads through, following fields, indexing and unwrapping.
    fn root_local(expr: &hir::Expr) -> Option<LocalId> {
        match &expr.kind {
            K::Local(local) => Some(*local),
            K::Field { object, .. } => Self::root_local(object),
            K::Index { container, .. } => Self::root_local(container),
            K::TupleGet { tuple, .. } => Self::root_local(tuple),
            K::Builtin { op: Op::OptUnwrap, args } => Self::root_local(&args[0]),
            _ => None,
        }
    }

    /// Report a value protected by a lock leaving its `with` block. Shareable values taken out
    /// of it may leave freely.
    pub(super) fn check_escape(&mut self, cx: &FnCx, value: &hir::Expr, what: &str) {
        let Some(local) = Self::root_local(value) else { return };
        if !cx.guarded.contains(&local) || self.shareable(value.ty) {
            return;
        }
        let name = cx.locals[local.0].name.clone();
        self.error(
            Diagnostic::error(
                codes::GUARD_ESCAPE,
                format!("a value protected by a mutex cannot be {what}"),
                value.span,
                format!("reached through `{name}`, which is only valid while the lock is held"),
            )
            .with_help("copy what you need while holding the lock, or keep the work inside the `with` block"),
        );
    }

    /// Whether a name, read from inside a closure, is a lock guard of an enclosing function.
    pub(super) fn captures_guard(cx: &FnCx, name: &str) -> bool {
        let mut outer = cx.outer.as_deref();
        while let Some(scope) = outer {
            if let Some(local) = scope.scope.get(name) {
                return scope.guarded.contains(local);
            }
            outer = scope.outer.as_deref();
        }
        false
    }
}
