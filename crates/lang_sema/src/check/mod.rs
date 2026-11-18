//! The type checker (docs/02-type-system-and-traits.md, "Typing rules"): every parameter and
//! field has a type hint, locals take their type from their first assignment, and any mismatch is
//! an error.
//!
//! Passes, in order:
//! 1. Declare every struct and constant, so types can refer to structs defined later.
//! 2. Resolve struct field types.
//! 3. Collect function and method signatures, so calls can refer to functions defined later.
//! 4. Check constant values and field defaults.
//! 5. Check that `main` exists and has the right shape.
//! 6. Check every body, producing HIR.
//!
//! Split across files: statements (`stmt.rs`), expressions (`expr.rs`), and built-in functions and
//! methods (`builtins.rs`).

mod builtins;
mod expr;
mod stmt;

use crate::hir::{self, FuncId, LocalId};
use crate::types::{StructId, Ty, TyKind, Types};
use lang_diag::{Diagnostic, Diagnostics, Span, codes};
use lang_syntax::ast;
use std::collections::HashMap;

/// Functions provided by the compiler rather than by source code.
const BUILTIN_FUNCTIONS: &[&str] = &["print", "len", "str", "int", "float", "abs", "panic", "range"];

/// Built-in type names, which cannot be redefined.
const BUILTIN_TYPES: &[&str] = &["bool", "list", "dict", "tuple", "set", "bytes", "None"];

/// Type names that exist in the language design but are not implemented yet.
const PLANNED_TYPES: &[&str] = &[
    "set",
    "bytes",
    "bytearray",
    "frozenset",
    "range",
    "Result",
    "Option",
    "i8",
    "i16",
    "i32",
    "u8",
    "u16",
    "u32",
    "u64",
    "f32",
];

const KNOWN_TYPES: &[&str] = &["int", "bool", "str", "float", "list", "dict", "tuple", "None"];

/// Check a parsed module. Returns HIR when this phase found no errors.
pub fn check(module: &ast::Module, module_name: &str, diags: &mut Diagnostics) -> Option<hir::Program> {
    let errors_before = diags.error_count();
    let mut checker = Checker {
        diags,
        types: Types::new(),
        structs: Vec::new(),
        globals: HashMap::new(),
        signatures: Vec::new(),
        bodies: Vec::new(),
        methods: HashMap::new(),
        consts: Vec::new(),
        struct_items: Vec::new(),
        const_items: Vec::new(),
    };
    checker.declare(module);
    checker.resolve_fields();
    checker.collect_functions(module);
    checker.check_consts();
    checker.check_field_defaults();
    let entry = checker.check_main(module);
    let functions: Vec<hir::Function> =
        (0..checker.bodies.len()).map(|index| checker.function(FuncId(index))).collect();
    if checker.diags.error_count() > errors_before {
        return None;
    }
    let structs = checker
        .structs
        .iter()
        .map(|s| hir::StructDef {
            name: s.name.clone(),
            ty: s.ty,
            fields: s.fields.iter().map(|f| hir::FieldDef { name: f.name.clone(), ty: f.ty }).collect(),
        })
        .collect();
    Some(hir::Program { module: module_name.to_string(), types: checker.types, structs, functions, entry: entry? })
}

#[derive(Clone, Copy)]
enum Global {
    Function(FuncId),
    Struct(StructId),
    Const(usize),
}

struct StructInfo {
    name: String,
    span: Span,
    ty: Ty,
    fields: Vec<FieldInfo>,
}

#[derive(Clone)]
struct FieldInfo {
    name: String,
    /// The field's type hint.
    span: Span,
    ty: Ty,
    default: Option<hir::Expr>,
}

struct Signature {
    /// `name`, or `Type.name` for methods.
    name: String,
    name_span: Span,
    params: Vec<ParamSig>,
    ret: Ty,
    ret_span: Option<Span>,
    /// For methods taking `self`: the type of `self`, and where it is written.
    receiver: Option<(Ty, Span)>,
}

#[derive(Clone)]
struct ParamSig {
    name: String,
    ty: Ty,
    /// The type hint, or the parameter name when the hint is missing.
    hint_span: Span,
}

struct ConstInfo {
    /// `None` when the constant's own definition had an error.
    value: Option<hir::Expr>,
}

struct Checker<'a, 'm> {
    diags: &'a mut Diagnostics,
    types: Types,
    /// Indexed by [`StructId`].
    structs: Vec<StructInfo>,
    globals: HashMap<String, (Global, Span)>,
    /// Indexed by [`FuncId`], in declaration order; methods follow the order of their `impl`.
    signatures: Vec<Signature>,
    bodies: Vec<&'m ast::Function>,
    methods: HashMap<(StructId, String), FuncId>,
    consts: Vec<ConstInfo>,
    struct_items: Vec<(StructId, &'m ast::StructDecl)>,
    const_items: Vec<(usize, &'m ast::ConstDecl)>,
}

/// Per-function state while checking a body.
struct FnCx {
    name: String,
    ret: Ty,
    ret_span: Option<Span>,
    locals: Vec<hir::Local>,
    info: Vec<LocalInfo>,
    scope: HashMap<String, LocalId>,
    /// Definite assignment: whether each local has a value on every path to the current point.
    assigned: Vec<bool>,
    /// Where each name is first assigned anywhere in the body, so that a read before that point
    /// can say "used before it is assigned" rather than "unknown name".
    first_assignment: HashMap<String, Span>,
    /// One entry per enclosing loop: whether it contains a `break`.
    loops: Vec<bool>,
}

struct LocalInfo {
    decl_span: Span,
    read: bool,
    is_param: bool,
    /// Compiler-generated temporaries, never shown to the user.
    hidden: bool,
}

impl FnCx {
    fn new(name: &str, ret: Ty, ret_span: Option<Span>) -> FnCx {
        FnCx {
            name: name.to_string(),
            ret,
            ret_span,
            locals: Vec::new(),
            info: Vec::new(),
            scope: HashMap::new(),
            assigned: Vec::new(),
            first_assignment: HashMap::new(),
            loops: Vec::new(),
        }
    }

    /// A context with no locals, for constant values and field defaults.
    fn detached() -> FnCx {
        FnCx::new("", Ty::NONE, None)
    }

    fn add_local(&mut self, name: &str, ty: Ty, decl_span: Span, is_param: bool) -> LocalId {
        let id = LocalId(self.locals.len());
        self.locals.push(hir::Local { name: name.to_string(), ty });
        self.info.push(LocalInfo { decl_span, read: false, is_param, hidden: false });
        self.assigned.push(false);
        self.scope.insert(name.to_string(), id);
        id
    }

    /// A hidden local holding an intermediate value, already assigned.
    fn temp(&mut self, ty: Ty, span: Span) -> LocalId {
        let id = LocalId(self.locals.len());
        self.locals.push(hir::Local { name: format!("tmp{}", id.0), ty });
        self.info.push(LocalInfo { decl_span: span, read: true, is_param: false, hidden: true });
        self.assigned.push(true);
        id
    }

    fn is_assigned(&self, id: LocalId) -> bool {
        self.assigned.get(id.0).copied().unwrap_or(false)
    }

    fn set_assigned(&mut self, id: LocalId) {
        if self.assigned.len() <= id.0 {
            self.assigned.resize(id.0 + 1, false);
        }
        self.assigned[id.0] = true;
    }
}

/// Record the first assignment of every name in a body, including inside nested blocks.
fn prescan(stmts: &[ast::Stmt], out: &mut HashMap<String, Span>) {
    fn target(t: &ast::Target, out: &mut HashMap<String, Span>) {
        match &t.kind {
            ast::TargetKind::Name(id) => {
                out.entry(id.name.clone()).or_insert(id.span);
            }
            ast::TargetKind::Tuple(items) => items.iter().for_each(|t| target(t, out)),
            _ => {}
        }
    }
    for stmt in stmts {
        match &stmt.kind {
            ast::StmtKind::Assign { target: t, .. } => target(t, out),
            ast::StmtKind::If { branches, otherwise } => {
                branches.iter().for_each(|(_, body)| prescan(body, out));
                if let Some(body) = otherwise {
                    prescan(body, out);
                }
            }
            ast::StmtKind::While { body, .. } => prescan(body, out),
            ast::StmtKind::For { target: t, body, .. } => {
                target(t, out);
                prescan(body, out);
            }
            _ => {}
        }
    }
}

/// What set an expected type, shown as a secondary label when a value does not match it.
type Reason = Option<(Span, String)>;

fn error_expr(span: Span) -> hir::Expr {
    hir::Expr { kind: hir::ExprKind::None, ty: Ty::ERROR, span }
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 { format!("{n} {word}") } else { format!("{n} {word}s") }
}

/// The candidate closest to `name` by edit distance, if it is close enough to be a typo.
fn closest<'a>(name: &str, candidates: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let limit = (name.chars().count() / 3).max(1);
    candidates
        .filter(|candidate| *candidate != name)
        .map(|candidate| (edit_distance(name, candidate), candidate))
        .filter(|(distance, _)| *distance <= limit)
        .min()
        .map(|(_, candidate)| candidate)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut current = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let substitution = previous[j] + usize::from(ca != *cb);
            current.push(substitution.min(previous[j + 1] + 1).min(current[j] + 1));
        }
        previous = current;
    }
    previous[b.len()]
}

impl<'m> Checker<'_, 'm> {
    fn error(&mut self, diagnostic: Diagnostic) {
        self.diags.push(diagnostic);
    }

    fn display(&self, ty: Ty) -> String {
        self.types.display(ty)
    }

    // ---- declarations -------------------------------------------------------------------

    /// Register a new top-level name. Returns false (after reporting) if it is taken.
    fn claim_global(&mut self, name: &ast::Ident) -> bool {
        if BUILTIN_FUNCTIONS.contains(&name.name.as_str())
            || BUILTIN_TYPES.contains(&name.name.as_str())
            || KNOWN_TYPES.contains(&name.name.as_str())
        {
            self.error(Diagnostic::error(
                codes::DUPLICATE_DEFINITION,
                format!("`{}` is a built-in name and cannot be redefined", name.name),
                name.span,
                "redefines a built-in",
            ));
            return false;
        }
        if let Some(&(_, first)) = self.globals.get(&name.name) {
            self.error(
                Diagnostic::error(
                    codes::DUPLICATE_DEFINITION,
                    format!("`{}` is defined more than once", name.name),
                    name.span,
                    "defined again here",
                )
                .with_label(first, "first defined here"),
            );
            return false;
        }
        true
    }

    fn declare(&mut self, module: &'m ast::Module) {
        for item in &module.items {
            match item {
                ast::Item::Struct(decl) => {
                    if !self.claim_global(&decl.name) {
                        continue;
                    }
                    let (id, ty) = self.types.add_struct(&decl.name.name);
                    self.structs.push(StructInfo {
                        name: decl.name.name.clone(),
                        span: decl.name.span,
                        ty,
                        fields: Vec::new(),
                    });
                    self.globals.insert(decl.name.name.clone(), (Global::Struct(id), decl.name.span));
                    self.struct_items.push((id, decl));
                }
                ast::Item::Const(decl) => {
                    if !self.claim_global(&decl.name) {
                        continue;
                    }
                    let index = self.consts.len();
                    self.consts.push(ConstInfo { value: None });
                    self.globals.insert(decl.name.name.clone(), (Global::Const(index), decl.name.span));
                    self.const_items.push((index, decl));
                }
                ast::Item::Function(_) | ast::Item::Impl(_) => {}
            }
        }
    }

    fn resolve_fields(&mut self) {
        for (id, decl) in self.struct_items.clone() {
            let mut fields: Vec<FieldInfo> = Vec::new();
            for field in &decl.fields {
                if let Some(first) = fields.iter().find(|f| f.name == field.name.name) {
                    let first_span = first.span;
                    self.error(
                        Diagnostic::error(
                            codes::DUPLICATE_DEFINITION,
                            format!("field `{}` is declared twice", field.name.name),
                            field.name.span,
                            "declared again here",
                        )
                        .with_label(first_span, "first declared here"),
                    );
                    continue;
                }
                let ty = self.resolve_type(&field.ty);
                if ty == Ty::NONE {
                    self.error(Diagnostic::error(
                        codes::NONE_VARIABLE,
                        format!("field `{}` cannot have type `None`", field.name.name),
                        field.ty.span,
                        "a `None` field carries no information",
                    ));
                }
                fields.push(FieldInfo { name: field.name.name.clone(), span: field.ty.span, ty, default: None });
            }
            self.structs[id.0].fields = fields;
        }
    }

    fn collect_functions(&mut self, module: &'m ast::Module) {
        for item in &module.items {
            match item {
                ast::Item::Function(function) => {
                    if let Some(span) = function.self_param {
                        self.error(
                            Diagnostic::error(
                                codes::UNEXPECTED_TOKEN,
                                "`self` is only allowed in methods",
                                span,
                                "not inside an `impl` block",
                            )
                            .with_help("define methods inside `impl TypeName:`"),
                        );
                    }
                    if !self.claim_global(&function.name) {
                        continue;
                    }
                    let id = FuncId(self.signatures.len());
                    let signature = self.signature(function, &function.name.name, None);
                    self.signatures.push(signature);
                    self.bodies.push(function);
                    self.globals.insert(function.name.name.clone(), (Global::Function(id), function.name.span));
                }
                ast::Item::Impl(imp) => self.collect_methods(imp),
                ast::Item::Struct(_) | ast::Item::Const(_) => {}
            }
        }
    }

    fn collect_methods(&mut self, imp: &'m ast::ImplDecl) {
        let owner = match self.globals.get(&imp.target.name) {
            Some(&(Global::Struct(id), _)) => id,
            _ => {
                let message = if KNOWN_TYPES.contains(&imp.target.name.as_str()) {
                    format!("cannot add methods to the built-in type `{}`", imp.target.name)
                } else {
                    format!("`impl {}` needs a struct named `{}` in this file", imp.target.name, imp.target.name)
                };
                self.error(Diagnostic::error(
                    codes::UNKNOWN_TYPE,
                    message,
                    imp.target.span,
                    "not a struct in this file",
                ));
                return;
            }
        };
        let owner_ty = self.structs[owner.0].ty;
        for method in &imp.methods {
            let name = &method.name.name;
            if let Some(&existing) = self.methods.get(&(owner, name.clone())) {
                let first = self.signatures[existing.0].name_span;
                self.error(
                    Diagnostic::error(
                        codes::DUPLICATE_DEFINITION,
                        format!("`{}` already has a method named `{name}`", imp.target.name),
                        method.name.span,
                        "defined again here",
                    )
                    .with_label(first, "first defined here"),
                );
                continue;
            }
            if let Some(field) = self.structs[owner.0].fields.iter().find(|f| &f.name == name) {
                let field_span = field.span;
                self.error(
                    Diagnostic::error(
                        codes::DUPLICATE_DEFINITION,
                        format!("`{}` already has a field named `{name}`", imp.target.name),
                        method.name.span,
                        "method with the same name as a field",
                    )
                    .with_label(field_span, "field declared here"),
                );
                continue;
            }
            let id = FuncId(self.signatures.len());
            let receiver = method.self_param.map(|span| (owner_ty, span));
            let signature = self.signature(method, &format!("{}.{name}", imp.target.name), receiver);
            self.signatures.push(signature);
            self.bodies.push(method);
            self.methods.insert((owner, name.clone()), id);
        }
    }

    fn signature(&mut self, function: &ast::Function, name: &str, receiver: Option<(Ty, Span)>) -> Signature {
        let mut params: Vec<ParamSig> = Vec::new();
        for param in &function.params {
            if let Some(previous) = params.iter().find(|p| p.name == param.name.name) {
                let previous_span = previous.hint_span;
                self.error(
                    Diagnostic::error(
                        codes::DUPLICATE_DEFINITION,
                        format!("parameter `{}` is listed twice", param.name.name),
                        param.name.span,
                        "listed again here",
                    )
                    .with_label(previous_span, "first listed here"),
                );
            }
            let (ty, hint_span) = match &param.ty {
                Some(hint) => {
                    let ty = self.resolve_type(hint);
                    if ty == Ty::NONE {
                        self.error(Diagnostic::error(
                            codes::NONE_PARAMETER,
                            format!("parameter `{}` cannot have type `None`", param.name.name),
                            hint.span,
                            "a `None` parameter carries no information",
                        ));
                    }
                    (ty, hint.span)
                }
                None => {
                    self.error(
                        Diagnostic::error(
                            codes::MISSING_TYPE_HINT,
                            format!("parameter `{}` has no type hint", param.name.name),
                            param.name.span,
                            "needs a type hint",
                        )
                        .with_help(format!("write the type after the name, for example `{}: int`", param.name.name)),
                    );
                    (Ty::ERROR, param.name.span)
                }
            };
            params.push(ParamSig { name: param.name.name.clone(), ty, hint_span });
        }
        let ret = function.ret.as_ref().map_or(Ty::NONE, |hint| self.resolve_type(hint));
        Signature {
            name: name.to_string(),
            name_span: function.name.span,
            params,
            ret,
            ret_span: function.ret.as_ref().map(|hint| hint.span),
            receiver,
        }
    }

    // ---- types --------------------------------------------------------------------------

    fn resolve_type(&mut self, hint: &ast::TypeExpr) -> Ty {
        match &hint.kind {
            ast::TypeExprKind::None => Ty::NONE,
            ast::TypeExprKind::Optional(inner) => {
                let inner_ty = self.resolve_type(inner);
                if inner_ty == Ty::NONE {
                    self.error(Diagnostic::error(
                        codes::UNKNOWN_TYPE,
                        "`None?` is not a useful type",
                        hint.span,
                        "write `None`",
                    ));
                    return Ty::ERROR;
                }
                if self.types.option_inner(inner_ty).is_some() {
                    self.error(Diagnostic::error(
                        codes::UNKNOWN_TYPE,
                        "an optional type cannot be optional again",
                        hint.span,
                        "remove one `?`",
                    ));
                    return Ty::ERROR;
                }
                self.types.option(inner_ty)
            }
            ast::TypeExprKind::Named { name, args } => self.resolve_named_type(name, args, hint.span),
        }
    }

    /// A type used inside another (an element or field type), where `None` is not allowed.
    fn resolve_value_type(&mut self, hint: &ast::TypeExpr) -> Ty {
        let ty = self.resolve_type(hint);
        if ty == Ty::NONE {
            self.error(
                Diagnostic::error(
                    codes::UNKNOWN_TYPE,
                    "`None` cannot be used as a type here",
                    hint.span,
                    "not a value type",
                )
                .with_help("to allow a missing value, use an optional type such as `int?`"),
            );
            return Ty::ERROR;
        }
        ty
    }

    fn resolve_named_type(&mut self, name: &str, args: &[ast::TypeExpr], span: Span) -> Ty {
        let arity = |expected: usize| args.len() == expected;
        match name {
            "int" | "bool" | "str" | "float" => {
                if !args.is_empty() {
                    self.error(Diagnostic::error(
                        codes::UNEXPECTED_TYPE_ARGS,
                        format!("the type `{name}` does not take type arguments"),
                        span,
                        "remove the `[...]`",
                    ));
                    return Ty::ERROR;
                }
                match name {
                    "int" => Ty::INT,
                    "bool" => Ty::BOOL,
                    "str" => Ty::STR,
                    _ => Ty::FLOAT,
                }
            }
            "list" if arity(1) => {
                let elem = self.resolve_value_type(&args[0]);
                self.types.list(elem)
            }
            "dict" if arity(2) => {
                let key = self.resolve_value_type(&args[0]);
                let value = self.resolve_value_type(&args[1]);
                if !self.types.is_hashable(key) {
                    self.unhashable(key, args[0].span);
                    return Ty::ERROR;
                }
                self.types.dict(key, value)
            }
            "tuple" if !args.is_empty() => {
                let items = args.iter().map(|a| self.resolve_value_type(a)).collect();
                self.types.tuple(items)
            }
            "list" | "dict" | "tuple" => {
                let expected = match name {
                    "list" => "`list[T]`, with one element type",
                    "dict" => "`dict[K, V]`, with a key type and a value type",
                    _ => "`tuple[A, B, ...]`, with at least one element type",
                };
                self.error(Diagnostic::error(
                    codes::UNEXPECTED_TYPE_ARGS,
                    format!("wrong number of type arguments for `{name}`"),
                    span,
                    format!("expected {expected}"),
                ));
                Ty::ERROR
            }
            planned if PLANNED_TYPES.contains(&planned) => {
                self.error(Diagnostic::error(
                    codes::NOT_SUPPORTED,
                    format!("the type `{planned}` is not supported yet"),
                    span,
                    "not supported yet",
                ));
                Ty::ERROR
            }
            other => match self.globals.get(other) {
                Some(&(Global::Struct(id), _)) => {
                    if !args.is_empty() {
                        self.error(Diagnostic::error(
                            codes::UNEXPECTED_TYPE_ARGS,
                            format!("the struct `{other}` does not take type arguments"),
                            span,
                            "remove the `[...]`",
                        ));
                    }
                    self.structs[id.0].ty
                }
                _ => {
                    self.unknown_type(other, span);
                    Ty::ERROR
                }
            },
        }
    }

    fn unhashable(&mut self, key: Ty, span: Span) {
        let shown = self.display(key);
        self.error(
            Diagnostic::error(
                codes::UNHASHABLE_KEY,
                format!("`{shown}` cannot be used as a dict key"),
                span,
                "not hashable",
            )
            .with_help("dict keys can be `int`, `str`, `bool`, or tuples of those"),
        );
    }

    fn unknown_type(&mut self, name: &str, span: Span) {
        let help = match name {
            "string" | "String" | "char" => Some("the string type is called `str`, as in Python".to_string()),
            "integer" | "Integer" | "long" | "i64" => Some("did you mean `int`?".to_string()),
            "boolean" | "Boolean" | "Bool" => Some("did you mean `bool`?".to_string()),
            "double" | "f64" | "Float" => Some("did you mean `float`?".to_string()),
            "List" | "Dict" | "Set" | "Tuple" => {
                Some(format!("built-in types are lower case: `{}`", name.to_lowercase()))
            }
            "Optional" => Some("write an optional type as `T?` or `T | None`".to_string()),
            "void" | "null" | "nil" | "NoneType" => {
                Some("a function that returns nothing needs no `->`; you can also write `-> None`".to_string())
            }
            _ => {
                let structs = self.structs.iter().map(|s| s.name.as_str());
                closest(name, KNOWN_TYPES.iter().copied().chain(structs)).map(|s| format!("did you mean `{s}`?"))
            }
        };
        let mut diagnostic =
            Diagnostic::error(codes::UNKNOWN_TYPE, format!("unknown type `{name}`"), span, "not a type");
        if let Some(help) = help {
            diagnostic = diagnostic.with_help(help);
        }
        self.error(diagnostic);
    }

    // ---- constants, defaults, main ------------------------------------------------------

    fn check_consts(&mut self) {
        for (index, decl) in self.const_items.clone() {
            let mut cx = FnCx::detached();
            let literal = match &decl.value.kind {
                ast::ExprKind::Int(_) | ast::ExprKind::Float(_) | ast::ExprKind::Str(_) | ast::ExprKind::Bool(_) => {
                    true
                }
                ast::ExprKind::Unary { op: ast::UnaryOp::Neg, operand } => {
                    matches!(operand.kind, ast::ExprKind::Int(_) | ast::ExprKind::Float(_))
                }
                _ => false,
            };
            if !literal {
                self.error(
                    Diagnostic::error(
                        codes::CONST_NOT_LITERAL,
                        format!("the value of constant `{}` must be a literal", decl.name.name),
                        decl.value.span,
                        "not a literal",
                    )
                    .with_help("use a number, string or `True`/`False`; computed constants are not supported yet"),
                );
                continue;
            }
            let value = match &decl.ty {
                Some(hint) => {
                    let ty = self.resolve_value_type(hint);
                    let reason = Some((hint.span, "expected because of this type hint".to_string()));
                    self.expect(&mut cx, &decl.value, ty, reason)
                }
                None => self.expr(&mut cx, &decl.value, None),
            };
            self.consts[index].value = Some(value);
        }
    }

    fn check_field_defaults(&mut self) {
        for (id, decl) in self.struct_items.clone() {
            for field in &decl.fields {
                let Some(default) = &field.default else { continue };
                let Some(index) = self.structs[id.0].fields.iter().position(|f| f.name == field.name.name) else {
                    continue;
                };
                let info = &self.structs[id.0].fields[index];
                let (ty, span) = (info.ty, info.span);
                let mut cx = FnCx::detached();
                let reason = Some((span, "expected because of the field's type".to_string()));
                let value = self.expect(&mut cx, default, ty, reason);
                self.structs[id.0].fields[index].default = Some(value);
            }
        }
    }

    fn check_main(&mut self, module: &ast::Module) -> Option<FuncId> {
        let Some(&(Global::Function(id), _)) = self.globals.get("main") else {
            self.error(
                Diagnostic::error(codes::NO_MAIN, "the program has no `main` function", module.start, "")
                    .with_help("add `def main():`; the program starts running there"),
            );
            return None;
        };
        let signature = &self.signatures[id.0];
        let (name_span, has_params, ret, ret_span) =
            (signature.name_span, !signature.params.is_empty(), signature.ret, signature.ret_span);
        if has_params {
            self.error(Diagnostic::error(
                codes::BAD_MAIN_SIGNATURE,
                "`main` cannot take parameters",
                name_span,
                "defined with parameters",
            ));
        }
        if let (Some(span), false) = (ret_span, matches!(ret, Ty::NONE | Ty::ERROR)) {
            self.error(Diagnostic::error(
                codes::BAD_MAIN_SIGNATURE,
                "`main` cannot return a value",
                span,
                "remove this return type",
            ));
        }
        Some(id)
    }

    // ---- bodies -------------------------------------------------------------------------

    fn function(&mut self, id: FuncId) -> hir::Function {
        let function = self.bodies[id.0];
        let signature = &self.signatures[id.0];
        let (name, params, ret, ret_span, receiver) =
            (signature.name.clone(), signature.params.clone(), signature.ret, signature.ret_span, signature.receiver);

        let mut cx = FnCx::new(&name, ret, ret_span);
        if let Some((ty, span)) = receiver {
            let local = cx.add_local("self", ty, span, true);
            cx.set_assigned(local);
        }
        for param in &params {
            let local = cx.add_local(&param.name, param.ty, param.hint_span, true);
            cx.set_assigned(local);
        }
        let param_count = cx.locals.len();
        prescan(&function.body, &mut cx.first_assignment);

        let (body, diverges) = self.block(&mut cx, &function.body);

        if !diverges && !matches!(ret, Ty::NONE | Ty::ERROR) {
            let shown = self.display(ret);
            let mut diagnostic = Diagnostic::error(
                codes::MISSING_RETURN,
                format!("function `{name}` can finish without returning a value"),
                function.name.span,
                "can reach its end without `return`",
            )
            .with_help("add a `return` statement at the end of the function");
            if let Some(span) = ret_span {
                diagnostic = diagnostic.with_label(span, format!("declared to return `{shown}`"));
            }
            self.error(diagnostic);
        }

        for (local, info) in cx.locals.iter().zip(&cx.info) {
            if !info.read && !info.is_param && !info.hidden && !local.name.starts_with('_') {
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

        hir::Function { name, params: (0..param_count).map(LocalId).collect(), locals: cx.locals, ret, body }
    }

    // ---- shared helpers -----------------------------------------------------------------

    /// What a top-level name is, for error messages.
    fn global_kind(&self, name: &str) -> Option<&'static str> {
        match self.globals.get(name) {
            Some((Global::Function(_), _)) => Some("a function"),
            Some((Global::Struct(_), _)) => Some("a struct"),
            Some((Global::Const(_), _)) => Some("a constant"),
            None if BUILTIN_FUNCTIONS.contains(&name) => Some("a built-in function"),
            None => None,
        }
    }

    fn struct_of(&self, ty: Ty) -> Option<StructId> {
        match self.types.kind(ty) {
            TyKind::Struct(id) => Some(*id),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lang_diag::FileId;

    fn check_text(text: &str) -> (Option<hir::Program>, Vec<&'static str>) {
        let mut diags = Diagnostics::default();
        let tokens = lang_syntax::lex(FileId(0), text, &mut diags);
        let module = lang_syntax::parse(&tokens, &mut diags);
        assert!(!diags.has_errors(), "test program does not parse");
        let program = check(&module, "test", &mut diags);
        (program, diags.sorted().iter().map(|d| d.code).collect())
    }

    fn ok(text: &str) -> hir::Program {
        let (program, codes) = check_text(text);
        assert!(codes.is_empty(), "unexpected diagnostics {codes:?}");
        program.unwrap()
    }

    fn errors(text: &str) -> Vec<&'static str> {
        check_text(text).1
    }

    #[test]
    fn hello_world_checks() {
        let program = ok("def main():\n    print(\"Hello, world!\")\n");
        assert_eq!(program.entry, FuncId(0));
    }

    #[test]
    fn locals_take_the_type_of_their_first_assignment() {
        let program = ok("def main():\n    x = 1\n    x = x + 2\n    print(x)\n");
        assert_eq!(program.functions[0].locals[0].ty, Ty::INT);
        assert_eq!(errors("def main():\n    x = 1\n    x = \"one\"\n    print(x)\n"), [codes::TYPE_MISMATCH]);
    }

    #[test]
    fn definite_assignment_follows_branches() {
        ok(
            "def f(c: bool):\n    if c:\n        x = 1\n    else:\n        x = 2\n    print(x)\n\ndef main():\n    f(True)\n",
        );
        assert_eq!(
            errors("def f(c: bool):\n    if c:\n        x = 1\n    print(x)\n\ndef main():\n    f(True)\n"),
            [codes::USED_BEFORE_ASSIGNED]
        );
        ok(
            "def f(c: bool):\n    if c:\n        x = 1\n    else:\n        return\n    print(x)\n\ndef main():\n    f(True)\n",
        );
        assert_eq!(
            errors("def main():\n    for i in range(3):\n        last = i\n    print(last)\n"),
            [codes::USED_BEFORE_ASSIGNED]
        );
    }

    #[test]
    fn missing_return_understands_branches_and_infinite_loops() {
        ok(
            "def f(c: bool) -> int:\n    if c:\n        return 1\n    else:\n        return 2\n\ndef main():\n    print(f(True))\n",
        );
        ok("def f() -> int:\n    while True:\n        return 1\n\ndef main():\n    print(f())\n");
        assert_eq!(
            errors("def f(c: bool) -> int:\n    if c:\n        return 1\n\ndef main():\n    print(f(True))\n"),
            [codes::MISSING_RETURN]
        );
        ok("def f() -> int:\n    panic(\"no\")\n\ndef main():\n    print(f())\n");
    }

    #[test]
    fn int_literals_adapt_to_float() {
        ok("def main():\n    x = 2.5\n    y = x * 2\n    z: float = 1\n    print(y + z, 1 + x)\n");
        assert_eq!(errors("def main():\n    n = 2\n    x = 2.5\n    print(n * x)\n"), [codes::BAD_OPERANDS]);
    }

    #[test]
    fn structs_and_methods() {
        let program = ok("\
struct Point:
    x: int
    y: int = 0

impl Point:
    def origin() -> Point:
        return Point(0, 0)

    def moved(self, dx: int) -> Point:
        return Point(x=self.x + dx, y=self.y)

def main():
    p = Point.origin().moved(3)
    p.y = 4
    print(p.x, p.y)
");
        assert_eq!(program.structs[0].fields.len(), 2);
        assert_eq!(
            errors("struct P:\n    x: int\n\ndef main():\n    p = P()\n    print(p.x)\n"),
            [codes::MISSING_FIELD]
        );
        assert_eq!(
            errors("struct P:\n    x: int\n\ndef main():\n    p = P(1)\n    print(p.z)\n"),
            [codes::NO_SUCH_MEMBER]
        );
    }

    #[test]
    fn collections_and_options() {
        ok("\
def main():
    words = [\"a\", \"b\"]
    counts: dict[str, int] = {}
    for w in words:
        counts[w] = counts.get_or(w, 0) + 1
    first: str? = None
    if len(words) > 0:
        first = words[0]
    for k, v in counts.items():
        print(k, v, first.unwrap_or(\"\"))
    pair = (1, \"x\")
    n, s = pair
    print(n, s, pair[1])
");
        assert_eq!(errors("def main():\n    xs = []\n    print(xs)\n"), [codes::CANNOT_INFER]);
        assert_eq!(errors("def main():\n    d: dict[float, int] = {}\n    print(d)\n"), [codes::UNHASHABLE_KEY]);
    }

    #[test]
    fn conditions_must_be_bool() {
        assert_eq!(errors("def main():\n    n = 1\n    if n:\n        print(n)\n"), [codes::NOT_BOOL]);
    }

    #[test]
    fn errors_do_not_cascade() {
        assert_eq!(errors("def main():\n    x = undefined\n    y = x + 1\n    print(y + x)\n"), [codes::UNKNOWN_NAME]);
    }

    #[test]
    fn format_specs_are_checked() {
        ok("def main():\n    x = 3.14159\n    print(f\"{x:.2f} {1:>4}\")\n");
        assert_eq!(errors("def main():\n    print(f\"{'a':.2f}\")\n"), [codes::BAD_FORMAT_SPEC]);
    }

    #[test]
    fn suggestions() {
        assert_eq!(closest("prnt", ["print", "main"].into_iter()), Some("print"));
        assert_eq!(closest("xyz", ["print"].into_iter()), None);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
    }
}
