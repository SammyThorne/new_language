//! Built-in functions (`print`, `len`, ...) and the methods of built-in types (`str`, `list`,
//! `dict`, optionals). Each is described by its parameter types and result type, then checked
//! like any other call.

use super::closures::CompKind;
use super::generics::Extra;
use super::{Checker, FnCx, Global, error_expr, plural};
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
    "encode",
];
const LIST_METHODS: &[&str] =
    &["append", "extend", "insert", "pop", "remove", "clear", "sort", "reverse", "index", "copy"];
const DICT_METHODS: &[&str] = &["get", "get_or", "pop", "keys", "values", "items", "clear"];
const SET_METHODS: &[&str] = &["add", "discard", "clear"];
const OPTION_METHODS: &[&str] = &["unwrap", "unwrap_or", "is_some", "is_none"];

/// The prefix of the prelude functions that are methods of a built-in type: `__str_count` is
/// `count` of a `str`.
fn prelude_prefix(kind: &TyKind) -> Option<&'static str> {
    Some(match kind {
        TyKind::Str => "str",
        TyKind::List(_) => "list",
        TyKind::Dict(_, value) if *value == Ty::NONE => "set",
        TyKind::Dict(..) => "dict",
        TyKind::FrozenList(_) | TyKind::Tuple(_) => "tuple",
        TyKind::FrozenDict(..) => "frozendict",
        TyKind::Int => "int",
        TyKind::Float => "float",
        TyKind::Bool => "bool",
        TyKind::Option(_) => "option",
        TyKind::Result(..) => "result",
        _ => return None,
    })
}

/// Python's names for methods that this language spells differently or has under another name.
const PYTHON_METHOD_NAMES: &[(&str, &str)] = &[
    ("startswith", "starts_with"),
    ("endswith", "ends_with"),
    ("splitlines", "split_lines"),
    ("isdigit", "is_digit"),
    ("isalpha", "is_alpha"),
    ("isalnum", "is_alnum"),
    ("isspace", "is_space"),
    ("isupper", "is_upper"),
    ("islower", "is_lower"),
    ("removeprefix", "remove_prefix"),
    ("removesuffix", "remove_suffix"),
    ("setdefault", "get_or_insert"),
    ("append", "add"),
    ("issubset", "is_subset"),
    ("issuperset", "is_superset"),
    ("isdisjoint", "is_disjoint"),
];
const RESULT_METHODS: &[&str] = &["unwrap", "unwrap_or", "unwrap_err", "is_ok", "is_err"];

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

    /// A set where a list of elements is wanted (`sorted(items)`, `sum(items)`): the same
    /// elements as a new list, in the order they were added.
    fn set_as_list(&mut self, value: hir::Expr) -> hir::Expr {
        let Some(elem) = self.types.set_elem(value.ty) else { return value };
        let ty = self.types.list(elem);
        let span = value.span;
        hir::Expr { kind: hir::ExprKind::Builtin { op: Op::DictKeys, args: vec![value] }, ty, span }
    }

    /// `list(x)`: a new list of the elements of a list, the characters of a string, the keys of a
    /// dict or the numbers of a `range(...)`.
    fn list_call(&mut self, cx: &mut FnCx, args: &[ast::Arg], span: Span) -> hir::Expr {
        self.elements_as_list(cx, "list", args, span)
    }

    /// `set()` makes an empty set of the type the context expects; `set(x)` the set of the
    /// elements of a list, a string, a dict's keys, a `range(...)` or another set.
    pub(super) fn set_call(&mut self, cx: &mut FnCx, args: &[ast::Arg], span: Span, expected: Option<Ty>) -> hir::Expr {
        if args.is_empty() {
            return match expected.and_then(|t| self.types.set_elem(t).map(|_| t)) {
                Some(ty) => hir::Expr { kind: hir::ExprKind::Dict(Vec::new()), ty, span },
                None if expected.is_some_and(|t| self.types.contains_error(t)) => error_expr(span),
                None => {
                    self.error(
                        Diagnostic::error(
                            codes::CANNOT_INFER,
                            "cannot infer the type of an empty set",
                            span,
                            "element type unknown",
                        )
                        .with_help("add a type hint, for example `items: set[int] = set()`"),
                    );
                    error_expr(span)
                }
            };
        }
        let list = self.elements_as_list(cx, "set", args, span);
        let Some(elem) = self.types.list_elem(list.ty) else { return error_expr(span) };
        if elem == Ty::ERROR {
            return error_expr(span);
        }
        if !self.hashable(elem) {
            self.unhashable(elem, args[0].value.span);
            return error_expr(span);
        }
        match self.globals.get("__set_of") {
            Some(&(Global::GenericFunction(index), _)) => {
                let func = self.instantiate_fn(index, vec![elem], span);
                let ty = self.types.set(elem);
                hir::Expr { kind: hir::ExprKind::Call { func, args: vec![list] }, ty, span }
            }
            _ => error_expr(span),
        }
    }

    /// The elements of a list, a string, a dict, a set or a `range(...)` as a new list: what
    /// `list(x)` is, and what `set(x)` starts from. `what` names the caller in messages.
    fn elements_as_list(&mut self, cx: &mut FnCx, what: &str, args: &[ast::Arg], span: Span) -> hir::Expr {
        if !self.no_keywords(args, &format!("`{what}`")) || !self.arity(what, args, 1, span) {
            self.check_all_quietly(cx, args);
            return error_expr(span);
        }
        let arg = &args[0].value;
        if let ast::ExprKind::Call { callee, .. } = &arg.kind
            && matches!(&callee.kind, ast::ExprKind::Name(n) if n == "range")
        {
            let name = ast::Ident { name: "__item".to_string(), span };
            let element = ast::Expr { kind: ast::ExprKind::Name(name.name.clone()), span };
            let target = ast::Target { kind: ast::TargetKind::Name(name), span };
            let clause = ast::CompClause { target, iter: arg.clone(), conditions: Vec::new() };
            return self.comprehension(cx, &element, CompKind::List, &[clause], span, None);
        }
        let value = self.expr(cx, arg, None);
        let builtin = |op: Op, value: hir::Expr, ty: Ty| hir::Expr {
            kind: hir::ExprKind::Builtin { op, args: vec![value] },
            ty,
            span,
        };
        match self.types.kind(value.ty).clone() {
            TyKind::List(elem) | TyKind::FrozenList(elem) => {
                let ty = self.types.list(elem);
                builtin(Op::ListCopy, value, ty)
            }
            TyKind::Dict(key, _) | TyKind::FrozenDict(key, _) => {
                let ty = self.types.list(key);
                builtin(Op::DictKeys, value, ty)
            }
            TyKind::Str => match self.globals.get("__chars") {
                Some(&(Global::Function(func), _)) => {
                    let ret = self.signatures[func.0].ret;
                    hir::Expr { kind: hir::ExprKind::Call { func, args: vec![value] }, ty: ret, span }
                }
                _ => error_expr(span),
            },
            TyKind::Error => error_expr(span),
            _ => {
                let shown = self.display(value.ty);
                self.error(
                    Diagnostic::error(
                        codes::NOT_ITERABLE,
                        format!("cannot make a {what} from a `{shown}`"),
                        arg.span,
                        format!("has type `{shown}`"),
                    )
                    .with_help(format!(
                        "`{what}(...)` takes a list, a string, a set, a dict (its keys) or `range(...)`"
                    )),
                );
                error_expr(span)
            }
        }
    }

    /// `print(a, b, sep="-", end="", flush=True)`: the keywords Python's `print` has, except
    /// `file`.
    fn print_with_keywords(&mut self, cx: &mut FnCx, args: &[ast::Arg], span: Span) -> hir::Expr {
        let string = |text: &str| hir::Expr { kind: hir::ExprKind::Str(text.to_string()), ty: Ty::STR, span };
        let mut values = Vec::new();
        let (mut sep, mut end, mut flush) = (None, None, None);
        let mut failed = false;
        for arg in args {
            let Some(name) = &arg.name else {
                let value = self.expr(cx, &arg.value, None);
                if value.ty != Ty::ERROR && !self.displayable(value.ty) {
                    self.not_displayable(value.ty, value.span);
                }
                values.push(value);
                continue;
            };
            let slot = match name.name.as_str() {
                "sep" | "end" | "flush" => None,
                other => {
                    let (message, help) = if other == "file" {
                        (
                            "`print` cannot write to a file".to_string(),
                            "write to standard error with `io.eprint(text)`, or to a file with `io.open`".to_string(),
                        )
                    } else {
                        (
                            format!("`print` has no keyword argument `{other}`"),
                            "its keyword arguments are `sep`, `end` and `flush`".to_string(),
                        )
                    };
                    self.error(
                        Diagnostic::error(codes::NOT_SUPPORTED, message, name.span, "not supported").with_help(help),
                    );
                    failed = true;
                    self.expr(cx, &arg.value, None);
                    Some(())
                }
            };
            if slot.is_some() {
                continue;
            }
            let (target, ty) = match name.name.as_str() {
                "sep" => (&mut sep, Ty::STR),
                "end" => (&mut end, Ty::STR),
                _ => (&mut flush, Ty::BOOL),
            };
            let value = self.expect(cx, &arg.value, ty, None);
            if target.is_some() {
                self.error(Diagnostic::error(
                    codes::NOT_SUPPORTED,
                    format!("`{}` is given twice", name.name),
                    name.span,
                    "repeated",
                ));
                failed = true;
            }
            *target = Some(value);
        }
        if failed {
            return error_expr(span);
        }
        values.push(sep.unwrap_or_else(|| string(" ")));
        values.push(end.unwrap_or_else(|| string("\n")));
        values.push(flush.unwrap_or(hir::Expr { kind: hir::ExprKind::Bool(false), ty: Ty::BOOL, span }));
        hir::Expr { kind: hir::ExprKind::Builtin { op: Op::PrintWith, args: values }, ty: Ty::NONE, span }
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
        if name == "sorted" {
            return self.sorted(cx, args, span);
        }
        if name == "list" {
            return self.list_call(cx, args, span);
        }
        if name == "tuple" || name == "frozendict" {
            return self.freeze_call(cx, name, args, span);
        }
        if matches!(name, "min" | "max" | "sum" | "enumerate" | "zip" | "any" | "all") {
            if !self.no_keywords(args, &format!("`{name}`")) {
                self.check_all_quietly(cx, args);
                return error_expr(span);
            }
            return self.collection_function(cx, name, args, span);
        }
        if name == "print" && args.iter().any(|a| a.name.is_some()) {
            return self.print_with_keywords(cx, args, span);
        }
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
                if value.ty != Ty::ERROR && !self.displayable(value.ty) {
                    self.not_displayable(value.ty, value.span);
                }
                values.push(value);
            }
            return builtin(Op::Print, values, Ty::NONE);
        }
        if name == "input" {
            if !self.arity(name, args, args.len().min(1), span) || args.len() > 1 {
                self.check_all_quietly(cx, args);
                return error_expr(span);
            }
            let prompt = match args.first() {
                Some(arg) => self.expect(cx, &arg.value, Ty::STR, None),
                None => hir::Expr { kind: hir::ExprKind::Str(String::new()), ty: Ty::STR, span },
            };
            let ty = self.types.option(Ty::STR);
            return builtin(Op::Input, vec![prompt], ty);
        }
        if name == "range" {
            self.error(
                Diagnostic::error(
                    codes::NOT_SUPPORTED,
                    "`range(...)` can only be used in a `for` loop for now",
                    span,
                    "outside a `for` loop",
                )
                .with_help("write `for i in range(n):`, or `list(range(n))` for a list of the numbers"),
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
        let numeric = matches!(kind, TyKind::Int | TyKind::Float | TyKind::Sized(_));
        if let Some(target) = crate::types::SizedKind::from_name(name) {
            if !numeric {
                let shown = self.display(value.ty);
                self.error(Diagnostic::error(
                    codes::TYPE_MISMATCH,
                    format!("`{name}` cannot take a `{shown}`"),
                    value.span,
                    "expected a number",
                ));
                return error_expr(span);
            }
            let ty = self.types.sized(target);
            return builtin(Op::NumConvert, vec![value], ty);
        }
        if let TyKind::Sized(_) = kind
            && matches!(name, "int" | "float")
        {
            let ty = if name == "int" { Ty::INT } else { Ty::FLOAT };
            return builtin(Op::NumConvert, vec![value], ty);
        }
        // `len(x)` on a type with a `len(self) -> int` method calls it, as `bytes` has.
        if name == "len"
            && matches!(kind, TyKind::Struct(_) | TyKind::Enum(_))
            && let Some(func) = self.method_of(value.ty, "len")
            && self.signatures[func.0].ret == Ty::INT
        {
            return hir::Expr { kind: hir::ExprKind::Call { func, args: vec![value] }, ty: Ty::INT, span };
        }
        let result = match (name, &kind) {
            (
                "len",
                TyKind::Str | TyKind::List(_) | TyKind::Dict(..) | TyKind::FrozenList(_) | TyKind::FrozenDict(..),
            ) => builtin(Op::Len, vec![value], Ty::INT),
            ("str", TyKind::Str) => value,
            ("str", _) if self.displayable(value.ty) => builtin(Op::ToStr, vec![value], Ty::STR),
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
                        format!("`{name}(...)` does not convert text to a number"),
                        value.span,
                        "a `str`",
                    )
                    .with_help(format!(
                        "parse it with `{name}.parse(text)`, which gives a `Result` because the text may not be a number"
                    )),
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

    /// `min`, `max`, `sum`, `enumerate`, `zip`, `any` and `all`.
    fn collection_function(&mut self, cx: &mut FnCx, name: &str, args: &[ast::Arg], span: Span) -> hir::Expr {
        let builtin =
            |op: Op, args: Vec<hir::Expr>, ty: Ty| hir::Expr { kind: hir::ExprKind::Builtin { op, args }, ty, span };
        let wrong = |this: &mut Self, value: &hir::Expr, accepted: &str| {
            let shown = this.display(value.ty);
            this.error(Diagnostic::error(
                codes::TYPE_MISMATCH,
                format!("`{name}` cannot take a `{shown}`"),
                value.span,
                format!("expected {accepted}"),
            ));
            error_expr(span)
        };
        match name {
            "min" | "max" if args.len() >= 2 => {
                let first = self.expr(cx, &args[0].value, None);
                let mut values = vec![first];
                for arg in &args[1..] {
                    let ty = values[0].ty;
                    values.push(self.expect(cx, &arg.value, ty, None));
                }
                let ty = values[0].ty;
                if ty == Ty::ERROR {
                    return error_expr(span);
                }
                if !self.orderable(ty) {
                    let first = values.swap_remove(0);
                    return wrong(self, &first, "values that can be ordered with `<`");
                }
                builtin(if name == "min" { Op::Min } else { Op::Max }, values, ty)
            }
            "min" | "max" | "sum" | "enumerate" | "any" | "all" => {
                let expected_args = if name == "sum" { args.len().clamp(1, 2) } else { 1 };
                if !self.arity(name, args, expected_args, span) {
                    self.check_all_quietly(cx, args);
                    return error_expr(span);
                }
                let hint = matches!(name, "any" | "all").then(|| self.types.list(Ty::BOOL));
                let list = self.expr(cx, &args[0].value, hint);
                let list = self.set_as_list(list);
                if list.ty == Ty::ERROR {
                    return error_expr(span);
                }
                let TyKind::List(elem) = *self.types.kind(list.ty) else {
                    return wrong(self, &list, "a list");
                };
                match name {
                    "min" | "max" => {
                        if !self.orderable(elem) {
                            return wrong(self, &list, "a list of values that can be ordered with `<`");
                        }
                        builtin(if name == "min" { Op::MinList } else { Op::MaxList }, vec![list], elem)
                    }
                    "sum" => {
                        if !matches!(elem, Ty::INT | Ty::FLOAT) {
                            return wrong(self, &list, "a list of `int` or `float`");
                        }
                        let mut values = vec![list];
                        if let Some(start) = args.get(1) {
                            values.push(self.expect(cx, &start.value, elem, None));
                        }
                        builtin(Op::Sum, values, elem)
                    }
                    "enumerate" => {
                        let pair = self.types.tuple(vec![Ty::INT, elem]);
                        let ty = self.types.list(pair);
                        builtin(Op::Enumerate, vec![list], ty)
                    }
                    _ => {
                        if elem != Ty::BOOL {
                            return wrong(self, &list, "a `list[bool]`");
                        }
                        builtin(if name == "any" { Op::Any } else { Op::All }, vec![list], Ty::BOOL)
                    }
                }
            }
            _ => {
                if !self.arity(name, args, 2, span) {
                    self.check_all_quietly(cx, args);
                    return error_expr(span);
                }
                let a = self.expr(cx, &args[0].value, None);
                let b = self.expr(cx, &args[1].value, None);
                if a.ty == Ty::ERROR || b.ty == Ty::ERROR {
                    return error_expr(span);
                }
                let (TyKind::List(x), TyKind::List(y)) = (self.types.kind(a.ty).clone(), self.types.kind(b.ty).clone())
                else {
                    let bad = if matches!(self.types.kind(a.ty), TyKind::List(_)) { b } else { a };
                    return wrong(self, &bad, "a list");
                };
                let pair = self.types.tuple(vec![x, y]);
                let ty = self.types.list(pair);
                builtin(Op::Zip, vec![a, b], ty)
            }
        }
    }

    /// `sorted(items, key=f, reverse=True)`: a sorted copy.
    fn sorted(&mut self, cx: &mut FnCx, args: &[ast::Arg], span: Span) -> hir::Expr {
        let positional: Vec<&ast::Arg> = args.iter().filter(|a| a.name.is_none()).collect();
        let [list_arg] = positional.as_slice() else {
            self.error(Diagnostic::error(
                codes::WRONG_ARG_COUNT,
                "`sorted` takes one list, and optionally `key=` and `reverse=`",
                span,
                "expected one list",
            ));
            self.check_all_quietly(cx, args);
            return error_expr(span);
        };
        let list = self.expr(cx, &list_arg.value, None);
        let list = self.set_as_list(list);
        if list.ty == Ty::ERROR {
            self.check_all_quietly(cx, args);
            return error_expr(span);
        }
        let TyKind::List(_) = self.types.kind(list.ty) else {
            let shown = self.display(list.ty);
            self.error(Diagnostic::error(
                codes::TYPE_MISMATCH,
                format!("`sorted` cannot take a `{shown}`"),
                list.span,
                "expected a list",
            ));
            return error_expr(span);
        };
        let keywords: Vec<&ast::Arg> = args.iter().filter(|a| a.name.is_some()).collect();
        let list_ty = list.ty;
        let copy = hir::Expr { kind: hir::ExprKind::Builtin { op: Op::ListCopy, args: vec![list] }, ty: list_ty, span };
        let temp = cx.temp(list_ty, span);
        let load = hir::Expr { kind: hir::ExprKind::Local(temp), ty: list_ty, span };
        let Some(sort) = self.sort_by(cx, load.clone(), &keywords, span) else { return error_expr(span) };
        hir::Expr {
            kind: hir::ExprKind::Block {
                stmts: vec![hir::Stmt::Assign { place: hir::Place::Local(temp), value: copy }, hir::Stmt::Expr(sort)],
                value: Box::new(load),
            },
            ty: list_ty,
            span,
        }
    }

    /// Sort `list` in place, with the `key=` and `reverse=` keyword arguments.
    fn sort_by(&mut self, cx: &mut FnCx, list: hir::Expr, keywords: &[&ast::Arg], span: Span) -> Option<hir::Expr> {
        let TyKind::List(elem) = *self.types.kind(list.ty) else { return None };
        let mut key: Option<hir::Expr> = None;
        let mut reverse: Option<hir::Expr> = None;
        for arg in keywords {
            let keyword = arg.name.as_ref().expect("keyword arguments");
            match keyword.name.as_str() {
                "key" => {
                    let value = match &arg.value.kind {
                        ast::ExprKind::Lambda { params, body } if params.len() == 1 => {
                            self.lambda_with(cx, params, body, vec![elem], None, arg.value.span)
                        }
                        _ => {
                            // The return type is only a placeholder: the key can be any orderable type.
                            let hint = self.types.func(vec![elem], Ty::ERROR);
                            self.expr(cx, &arg.value, Some(hint))
                        }
                    };
                    match self.types.kind(value.ty).clone() {
                        TyKind::Func(params, ret) if params == [elem] => {
                            // Code generation collects the keys in a list of this type.
                            self.types.list(ret);
                            if !self.orderable(ret) {
                                let shown = self.display(ret);
                                self.error(Diagnostic::error(
                                    codes::BAD_OPERANDS,
                                    format!("the sort key is a `{shown}`, which cannot be ordered"),
                                    arg.value.span,
                                    "keys must be comparable with `<`",
                                ));
                                return None;
                            }
                            key = Some(value);
                        }
                        TyKind::Error => return None,
                        _ => {
                            let (shown, want) = (self.display(value.ty), self.display(elem));
                            self.error(
                                Diagnostic::error(
                                    codes::TYPE_MISMATCH,
                                    format!("`key` must be a function taking a `{want}`, found `{shown}`"),
                                    arg.value.span,
                                    "not a key function",
                                )
                                .with_help("for example `key=lambda item: item.name`"),
                            );
                            return None;
                        }
                    }
                }
                "reverse" => reverse = Some(self.expect(cx, &arg.value, Ty::BOOL, None)),
                other => {
                    self.error(Diagnostic::error(
                        codes::NO_SUCH_MEMBER,
                        format!("sorting has no option `{other}`"),
                        keyword.span,
                        "the options are `key` and `reverse`",
                    ));
                    return None;
                }
            }
        }
        if key.is_none() && !self.orderable(elem) {
            let shown = self.display(elem);
            self.error(
                Diagnostic::error(
                    codes::BAD_OPERANDS,
                    format!("a list of `{shown}` cannot be sorted"),
                    span,
                    "elements have no order",
                )
                .with_help(format!("pass `key=` to sort by part of each value, or implement `Ord` for `{shown}`")),
            );
            return None;
        }
        let key = key.unwrap_or(hir::Expr { kind: hir::ExprKind::None, ty: Ty::NONE, span });
        let reverse = reverse.unwrap_or(hir::Expr { kind: hir::ExprKind::Bool(false), ty: Ty::BOOL, span });
        Some(hir::Expr {
            kind: hir::ExprKind::Builtin { op: Op::ListSortBy, args: vec![list, key, reverse] },
            ty: Ty::NONE,
            span,
        })
    }

    /// The trait methods that built-in types have: `to_str`, `eq`, `cmp`, `hash`, and the
    /// number traits' `add`, `sub` and `mul`. Generic code calls these through bounds.
    fn builtin_trait_method(
        &mut self,
        cx: &mut FnCx,
        receiver: hir::Expr,
        name: &ast::Ident,
        args: &[ast::Arg],
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let ty = receiver.ty;
        let n = args.len();
        let numeric = matches!(ty, Ty::INT | Ty::FLOAT);
        let applies = match name.name.as_str() {
            "to_str" => n == 0 && self.displayable(ty),
            "hash" => n == 0 && self.hashable(ty),
            "eq" => n == 1 && self.equatable(ty),
            "cmp" => n == 1 && self.orderable(ty),
            "add" | "sub" | "mul" => n == 1 && numeric,
            _ => false,
        };
        if !applies || args.iter().any(|a| a.name.is_some()) {
            return Err(receiver);
        }
        let other = args.first().map(|a| self.expect(cx, &a.value, ty, None));
        let make = |kind: hir::ExprKind, ty: Ty| hir::Expr { kind, ty, span };
        Ok(match (name.name.as_str(), other) {
            ("to_str", _) => make(hir::ExprKind::Builtin { op: Op::ToStr, args: vec![receiver] }, Ty::STR),
            ("hash", _) => make(hir::ExprKind::Builtin { op: Op::Hash, args: vec![receiver] }, Ty::INT),
            ("eq", Some(other)) => make(
                hir::ExprKind::Binary { op: hir::BinaryOp::Eq, lhs: Box::new(receiver), rhs: Box::new(other) },
                Ty::BOOL,
            ),
            ("cmp", Some(other)) => make(hir::ExprKind::Builtin { op: Op::Cmp, args: vec![receiver, other] }, Ty::INT),
            (method, Some(other)) => {
                let op = match (method, ty) {
                    ("add", Ty::INT) => hir::BinaryOp::IntAdd,
                    ("sub", Ty::INT) => hir::BinaryOp::IntSub,
                    ("mul", Ty::INT) => hir::BinaryOp::IntMul,
                    ("add", _) => hir::BinaryOp::FloatAdd,
                    ("sub", _) => hir::BinaryOp::FloatSub,
                    _ => hir::BinaryOp::FloatMul,
                };
                make(hir::ExprKind::Binary { op, lhs: Box::new(receiver), rhs: Box::new(other) }, ty)
            }
            _ => unreachable!("arity checked above"),
        })
    }

    /// The prelude function that is method `name` of a value of type `ty`, if there is one:
    /// `__str_count` is `count` of a `str`.
    pub(super) fn prelude_method(&self, ty: Ty, name: &str) -> Option<Global> {
        let prefix = prelude_prefix(self.types.kind(ty))?;
        match self.globals.get(&format!("__{prefix}_{name}")) {
            Some(&(global @ (Global::Function(_) | Global::GenericFunction(_)), _)) => Some(global),
            _ => None,
        }
    }

    /// The names of the prelude's methods of the type of `ty`.
    fn prelude_method_names(&self, ty: Ty) -> Vec<String> {
        let Some(prefix) = prelude_prefix(self.types.kind(ty)) else { return Vec::new() };
        let start = format!("__{prefix}_");
        let mut names: Vec<String> = self
            .globals
            .iter()
            .filter(|(_, (global, _))| matches!(global, Global::Function(_) | Global::GenericFunction(_)))
            .filter_map(|(name, _)| name.strip_prefix(&start).map(String::from))
            .collect();
        names.sort();
        names
    }

    /// A call of a method the prelude defines, with the receiver as the function's first argument.
    /// A last parameter named `__at` is filled in with the text of the call's location, so that
    /// a panic there points at the user's line, not the prelude's.
    fn call_prelude_method(
        &mut self,
        cx: &mut FnCx,
        receiver: hir::Expr,
        global: Global,
        name: &ast::Ident,
        args: &[ast::Arg],
        span: Span,
    ) -> hir::Expr {
        let prefix = prelude_prefix(self.types.kind(receiver.ty)).unwrap_or("value");
        let display = format!("{prefix}.{}", name.name);
        let location_expr = self.location_arg(span);
        let location = |wants: bool| wants.then(|| location_expr.clone());
        match global {
            Global::Function(func) => {
                let signature = self.signatures[func.0].clone();
                let location = location(signature.params.last().is_some_and(|p| p.name == "__at"));
                let extra = Extra { name: Some(display.clone()), receiver: Some(receiver), location };
                let written = extra.written(&signature.params);
                let mut all: Vec<hir::Expr> = extra.receiver.iter().cloned().collect();
                all.extend(self.call_args(cx, &display, signature.name_span, written, args, span));
                all.extend(extra.location);
                hir::Expr { kind: hir::ExprKind::Call { func, args: all }, ty: signature.ret, span }
            }
            Global::GenericFunction(index) => {
                let pattern = self.generic_fns[index].pattern.expect("generic functions have a pattern");
                let wants = self.signatures[pattern.0].params.last().is_some_and(|p| p.name == "__at");
                let extra = Extra { name: Some(display), receiver: Some(receiver), location: location(wants) };
                self.call_generic_with(cx, index, extra, args, span, None)
            }
            _ => unreachable!("prelude_method gives functions"),
        }
    }

    pub(super) fn is_builtin_method(&self, ty: Ty, name: &str) -> bool {
        if self.prelude_method(ty, name).is_some() {
            return true;
        }
        let methods = match self.types.kind(ty) {
            TyKind::Str => STR_METHODS,
            TyKind::List(_) => LIST_METHODS,
            TyKind::Dict(_, Ty::NONE) => SET_METHODS,
            TyKind::Dict(..) => DICT_METHODS,
            TyKind::Option(_) => OPTION_METHODS,
            TyKind::Result(..) => RESULT_METHODS,
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
        // `text.encode()`: the string's UTF-8 bytes, made by the prelude.
        if kind == TyKind::Str
            && name.name == "encode"
            && args.is_empty()
            && let Some(&(super::Global::Function(func), _)) = self.globals.get("__bytes")
        {
            let ret = self.signatures[func.0].ret;
            return hir::Expr { kind: hir::ExprKind::Call { func, args: vec![receiver] }, ty: ret, span };
        }
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
                    "remove" | "index" if !self.equatable(elem) => {
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
                    "sort" if !args.is_empty() || !self.orderable(elem) => {
                        if let Some(arg) = args.iter().find(|a| a.name.is_none()) {
                            self.error(Diagnostic::error(
                                codes::WRONG_ARG_COUNT,
                                "`sort` takes only the keyword arguments `key=` and `reverse=`",
                                arg.value.span,
                                "positional argument",
                            ));
                            self.check_all_quietly(cx, args);
                            return error_expr(span);
                        }
                        let keywords: Vec<&ast::Arg> = args.iter().collect();
                        return self.sort_by(cx, receiver, &keywords, span).unwrap_or_else(|| error_expr(span));
                    }
                    "sort" => Some((Op::ListSort, vec![], Ty::NONE)),
                    "reverse" => Some((Op::ListReverse, vec![], Ty::NONE)),
                    "copy" => Some((Op::ListCopy, vec![], ty)),
                    _ => None,
                }
            }
            (TyKind::FrozenList(elem), method) => {
                let elem = *elem;
                match method {
                    "index" if self.equatable(elem) => Some((Op::ListIndex, vec![elem], Ty::INT)),
                    "copy" => Some((Op::ListCopy, vec![], self.types.list(elem))),
                    _ => None,
                }
            }
            (TyKind::FrozenDict(key, value), method) => {
                let (key, value) = (*key, *value);
                match method {
                    "get" => Some((Op::DictGet, vec![key], self.types.option(value))),
                    "get_or" => Some((Op::DictGetOr, vec![key, value], value)),
                    "keys" => Some((Op::DictKeys, vec![], self.types.list(key))),
                    "values" => Some((Op::DictValues, vec![], self.types.list(value))),
                    "items" => {
                        let pair = self.types.tuple(vec![key, value]);
                        Some((Op::DictItems, vec![], self.types.list(pair)))
                    }
                    _ => None,
                }
            }
            (TyKind::Dict(elem, Ty::NONE), method) => {
                let elem = *elem;
                match method {
                    "add" => Some((Op::SetAdd, vec![elem], Ty::NONE)),
                    "discard" => Some((Op::SetDiscard, vec![elem], Ty::BOOL)),
                    "clear" => Some((Op::DictClear, vec![], Ty::NONE)),
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
            (TyKind::Result(ok, err), method) => {
                let (ok, err) = (*ok, *err);
                match method {
                    "unwrap" => Some((Op::ResultUnwrap, vec![], ok)),
                    "unwrap_or" => Some((Op::ResultUnwrapOr, vec![ok], ok)),
                    "unwrap_err" => Some((Op::ResultUnwrapErr, vec![], err)),
                    "is_ok" => Some((Op::ResultIsOk, vec![], Ty::BOOL)),
                    "is_err" => Some((Op::ResultIsErr, vec![], Ty::BOOL)),
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

        // A method the prelude defines (docs/11, milestone 2) is used when the compiler has none
        // of that name, or none that takes these arguments, as `strip()` and `strip("x")`.
        if let Some(global) = self.prelude_method(ty, &name.name) {
            let overloaded = match &signature {
                None => true,
                Some((_, params, _)) => n != params.len() || args.iter().any(|a| a.name.is_some()),
            };
            if overloaded {
                return self.call_prelude_method(cx, receiver, global, name, args, span);
            }
        }
        let Some((op, params, ret)) = signature else {
            return match self.builtin_trait_method(cx, receiver, name, args, span) {
                Ok(value) => value,
                Err(_) => self.unknown_builtin_method(cx, ty, &kind, name, args, span),
            };
        };
        if !self.no_keywords(args, &format!("`{}`", name.name)) || !self.arity(&name.name, args, params.len(), span) {
            for arg in args {
                self.expr(cx, &arg.value, None);
            }
            return error_expr(span);
        }
        let mut all = vec![receiver];
        for (arg, param) in args.iter().zip(params) {
            let value = self.expect(cx, &arg.value, param, None);
            if matches!(op, Op::ListAppend | Op::ListInsert | Op::ListExtend | Op::SetAdd) {
                self.check_escape(cx, &value, "stored in a collection");
            }
            all.push(value);
        }
        hir::Expr { kind: hir::ExprKind::Builtin { op, args: all }, ty: ret, span }
    }

    fn unknown_builtin_method(
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
            TyKind::Dict(_, Ty::NONE) => SET_METHODS,
            TyKind::Dict(..) => DICT_METHODS,
            TyKind::Result(..) => RESULT_METHODS,
            TyKind::Option(_) => OPTION_METHODS,
            TyKind::FrozenList(_) => &["index", "copy"],
            TyKind::FrozenDict(..) => &["get", "get_or", "keys", "values", "items"],
            _ => &[],
        };
        let prelude_names = self.prelude_method_names(ty);
        let help = match name.name.as_str() {
            "unwrap" | "unwrap_or" | "is_some" | "is_none" => None,
            python
                if PYTHON_METHOD_NAMES.iter().any(|(from, to)| {
                    *from == python && (methods.contains(to) || prelude_names.iter().any(|n| n == to))
                }) =>
            {
                let (_, to) = PYTHON_METHOD_NAMES.iter().find(|(from, _)| *from == python).unwrap();
                Some(format!("the method is called `{to}`"))
            }
            other => super::closest(other, methods.iter().copied().chain(prelude_names.iter().map(String::as_str)))
                .map(|s| format!("did you mean `{s}`?")),
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
