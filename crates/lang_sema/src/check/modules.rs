//! Modules: one scope per file, `import` and `from ... import`, names qualified by a module
//! (`models.User`, `io.read_file(path)`), and the visibility rules of docs/01 ("Visibility").
//!
//! Every module's scope starts with the prelude's declarations. A module's own top-level
//! declarations are recorded with whether they are `pub`; importing a name, or reaching it as
//! `module.name`, requires that. Members (fields and methods) marked `priv` are visible only in
//! the file that defines their type.
//!
//! The checker holds the current module's scope in `globals`, and swaps it when it moves to a
//! declaration or body from another file ([`Checker::enter_module`]).

use super::{Checker, FnCx, Global, ParamSig, Signature, closest, error_expr};
use crate::hir::{self, LocalId};
use crate::types::{Ty, TyKind};
use lang_diag::{Diagnostic, Span, codes};
use lang_syntax::ast;

impl<'m> Checker<'_, 'm> {
    pub(super) fn start_modules(&mut self, modules: &[(String, &ast::Module)]) {
        for (index, (name, module)) in modules.iter().enumerate() {
            self.module_names.push(name.clone());
            self.module_scopes.push(Default::default());
            self.exports.push(Default::default());
            self.file_module.insert(module.start.file, index);
        }
    }

    /// Make module `index` the current one: its scope becomes `globals`.
    pub(super) fn enter_module(&mut self, index: usize) {
        if index == self.current_module {
            return;
        }
        let scope = std::mem::take(&mut self.globals);
        self.module_scopes[self.current_module] = scope;
        self.globals = std::mem::take(&mut self.module_scopes[index]);
        self.current_module = index;
        if self.globals.is_empty() && index != 0 {
            // A new module's scope starts with the prelude.
            self.globals = self.exports[0].iter().map(|(name, (g, span, _))| (name.clone(), (*g, *span))).collect();
        }
    }

    /// Run `body` in module `index`, then return to the current one.
    pub(super) fn in_module<R>(&mut self, index: usize, body: impl FnOnce(&mut Self) -> R) -> R {
        let saved = self.current_module;
        self.enter_module(index);
        let result = body(self);
        self.enter_module(saved);
        result
    }

    /// The module a declaration belongs to, from the file its span is in.
    pub(super) fn module_of(&self, span: Span) -> usize {
        self.file_module.get(&span.file).copied().unwrap_or(0)
    }

    pub(super) fn is_entry_module(&self, index: usize) -> bool {
        index + 1 == self.module_names.len()
    }

    /// The name a function gets in HIR and in diagnostics: qualified by its module, except in
    /// the prelude and the program's main file.
    pub(super) fn qualified(&self, name: &str) -> String {
        let module = self.current_module;
        if module == 0 || self.is_entry_module(module) {
            name.to_string()
        } else {
            format!("{}.{name}", self.module_names[module])
        }
    }

    /// Record a top-level declaration of the current module.
    pub(super) fn export(&mut self, name: &ast::Ident, global: Global, visibility: ast::Visibility) {
        let public = visibility == ast::Visibility::Pub || self.current_module == 0;
        self.exports[self.current_module].insert(name.name.clone(), (global, name.span, public));
    }

    // ---- imports ------------------------------------------------------------------------

    pub(super) fn import_all(&mut self, modules: &[(String, &'m ast::Module)]) {
        for (index, (_, module)) in modules.iter().enumerate().skip(1) {
            self.enter_module(index);
            for item in &module.items {
                if let ast::Item::Import(import) = item {
                    self.import(import);
                }
            }
        }
    }

    fn import(&mut self, import: &ast::ImportDecl) {
        let path: Vec<&str> = import.path.iter().map(|p| p.name.as_str()).collect();
        let target_name = path.join(".");
        let span = import.path[0].span.to(import.path.last().expect("a path").span);
        let Some(target) = self.module_names.iter().position(|n| *n == target_name) else {
            // The loader reports modules it cannot find; this only happens for single-file checks.
            self.error(Diagnostic::error(
                codes::UNKNOWN_MODULE,
                format!("cannot find module `{target_name}`"),
                span,
                "no such module",
            ));
            return;
        };
        match &import.names {
            None => {
                let (key, key_span) = match &import.alias {
                    Some(alias) => (alias.name.clone(), alias.span),
                    None => (target_name.clone(), span),
                };
                self.bind_import(&key, key_span, Global::Module(target));
            }
            Some(names) => {
                for (name, alias) in names {
                    let global = self.member(target, name);
                    let (key, key_span) = match alias {
                        Some(alias) => (alias.name.clone(), alias.span),
                        None => (name.name.clone(), name.span),
                    };
                    self.bind_import(&key, key_span, global);
                }
            }
        }
    }

    fn bind_import(&mut self, key: &str, span: Span, global: Global) {
        if let Some(&(_, first)) = self.globals.get(key)
            && first.file != self.prelude_file
        {
            self.error(
                Diagnostic::error(
                    codes::DUPLICATE_DEFINITION,
                    format!("`{key}` is defined more than once"),
                    span,
                    "imported here",
                )
                .with_label(first, "also defined here")
                .with_help("import it under another name with `as`"),
            );
            return;
        }
        self.globals.insert(key.to_string(), (global, span));
    }

    /// A top-level declaration of module `target`, reached from the current module. Reports
    /// (and returns [`Global::Missing`]) if there is none, or if it is not `pub`, which is then
    /// still returned so that its uses are checked.
    pub(super) fn member(&mut self, target: usize, name: &ast::Ident) -> Global {
        let module = self.module_names[target].clone();
        match self.exports[target].get(&name.name).copied() {
            Some((global, _, true)) => global,
            Some((global, defined, false)) => {
                if target != self.current_module {
                    self.error(
                        Diagnostic::error(
                            codes::PRIVATE,
                            format!("`{}` is private to module `{module}`", name.name),
                            name.span,
                            "not `pub`",
                        )
                        .with_label(defined, "defined here")
                        .with_help(format!("mark it `pub` in `{module}` to use it from other files")),
                    );
                }
                global
            }
            None => {
                let mut diagnostic = Diagnostic::error(
                    codes::UNKNOWN_NAME,
                    format!("module `{module}` has no `{}`", name.name),
                    name.span,
                    "not found",
                );
                let public: Vec<&str> =
                    self.exports[target].iter().filter(|(_, (_, _, p))| *p).map(|(n, _)| n.as_str()).collect();
                if let Some(s) = closest(&name.name, public.into_iter()) {
                    diagnostic = diagnostic.with_help(format!("did you mean `{s}`?"));
                }
                self.error(diagnostic);
                Global::Missing
            }
        }
    }

    // ---- qualified names ----------------------------------------------------------------

    /// The dotted name an expression spells, if it is only names and attributes: `a.b.c`.
    pub(super) fn dotted(expr: &ast::Expr) -> Option<String> {
        match &expr.kind {
            ast::ExprKind::Name(name) => Some(name.clone()),
            ast::ExprKind::Attr { object, name } => Some(format!("{}.{}", Self::dotted(object)?, name.name)),
            _ => None,
        }
    }

    /// If `object` names an imported module, the global `object.name` refers to.
    pub(super) fn member_global(&mut self, cx: &FnCx, object: &ast::Expr, name: &ast::Ident) -> Option<Global> {
        let path = Self::dotted(object)?;
        let first = path.split('.').next().unwrap_or(&path);
        if cx.sees(first) {
            return None;
        }
        let Some(&(Global::Module(target), _)) = self.globals.get(&path) else { return None };
        Some(self.member(target, name))
    }

    /// The global an expression names: a name that is not a local, or `module.name`.
    pub(super) fn path_global(&mut self, cx: &FnCx, expr: &ast::Expr) -> Option<Global> {
        match &expr.kind {
            ast::ExprKind::Name(name) if !cx.sees(name) => self.globals.get(name).map(|(g, _)| *g),
            ast::ExprKind::Attr { object, name } => self.member_global(cx, object, name),
            _ => None,
        }
    }

    /// A type name, possibly qualified (`models.User`), to the global it names.
    pub(super) fn type_global(&mut self, name: &str, span: Span) -> Option<Global> {
        let Some((module, member)) = name.rsplit_once('.') else {
            return self.globals.get(name).map(|(g, _)| *g);
        };
        match self.globals.get(module).map(|(g, _)| *g) {
            Some(Global::Module(target)) => {
                let ident = ast::Ident { name: member.to_string(), span };
                Some(self.member(target, &ident))
            }
            _ => {
                self.error(
                    Diagnostic::error(
                        codes::UNKNOWN_MODULE,
                        format!("unknown module `{module}`"),
                        span,
                        "not an imported module",
                    )
                    .with_help(format!("add `import {module}` at the top of the file")),
                );
                Some(Global::Missing)
            }
        }
    }

    /// Whether an enum is reachable by name from the current module, so that its variants can
    /// be used without the enum's name.
    pub(super) fn enum_in_scope(&self, global: Global) -> bool {
        self.globals.values().any(|(g, _)| match (g, global) {
            (Global::Enum(a), Global::Enum(b)) => *a == b,
            (Global::GenericEnum(a), Global::GenericEnum(b)) => *a == b,
            _ => false,
        })
    }

    // ---- member visibility --------------------------------------------------------------

    /// Report reading or writing a `priv` field outside the file that defines its struct.
    pub(super) fn check_field_visible(&mut self, struct_ty: Ty, field: usize, span: Span) {
        let Some(id) = self.struct_of(struct_ty) else { return };
        let info = &self.structs[id.0];
        if !info.fields[field].private || self.module_of(info.span) == self.current_module {
            return;
        }
        let (struct_name, field_name, field_span) =
            (info.name.clone(), info.fields[field].name.clone(), info.fields[field].span);
        let module = self.module_names[self.module_of(info.span)].clone();
        self.error(
            Diagnostic::error(
                codes::PRIVATE,
                format!("field `{field_name}` of `{struct_name}` is private to module `{module}`"),
                span,
                "private field",
            )
            .with_label(field_span, "declared `priv` here"),
        );
    }

    /// Report constructing a struct outside its file when a `priv` field would need a value:
    /// one without a default, or one the arguments give.
    pub(super) fn check_constructible(&mut self, id: crate::types::StructId, args: &[ast::Arg], span: Span) -> bool {
        let info = &self.structs[id.0];
        if self.module_of(info.span) == self.current_module {
            return true;
        }
        let given = |index: usize, name: &str| {
            args.iter().enumerate().any(|(i, a)| match &a.name {
                Some(keyword) => keyword.name == name,
                None => i == index,
            })
        };
        let Some(field) = info
            .fields
            .iter()
            .enumerate()
            .find(|(i, f)| f.private && (f.default.is_none() || given(*i, &f.name)))
            .map(|(_, f)| f)
        else {
            return true;
        };
        let (struct_name, field_name, field_span) = (info.name.clone(), field.name.clone(), field.span);
        let module = self.module_names[self.module_of(info.span)].clone();
        self.error(
            Diagnostic::error(
                codes::PRIVATE,
                format!("`{struct_name}` cannot be created outside module `{module}`"),
                span,
                format!("its field `{field_name}` is private"),
            )
            .with_label(field_span, "declared `priv` here")
            .with_help(format!("use a function from `{module}` that creates one")),
        );
        false
    }

    /// Report calling a `priv` method outside the file that defines it.
    pub(super) fn check_method_visible(&mut self, func: hir::FuncId, span: Span) {
        let signature = &self.signatures[func.0];
        if !signature.private || self.module_of(signature.name_span) == self.current_module {
            return;
        }
        let (name, defined) = (signature.name.clone(), signature.name_span);
        let module = self.module_names[self.module_of(defined)].clone();
        self.error(
            Diagnostic::error(
                codes::PRIVATE,
                format!("method `{name}` is private to module `{module}`"),
                span,
                "private method",
            )
            .with_label(defined, "declared `priv` here"),
        );
    }

    // ---- extern functions ---------------------------------------------------------------

    /// Whether a type can cross the C boundary in an `@extern` function's signature. `str`
    /// crosses as a pointer and a length; a returned `str?` is `None` when the pointer is null.
    fn ffi_type(&self, ty: Ty, ret: bool) -> bool {
        match self.types.kind(ty) {
            TyKind::Int | TyKind::Float | TyKind::Bool | TyKind::Str | TyKind::Sized(_) | TyKind::Error => true,
            TyKind::None => ret,
            TyKind::Option(inner) => ret && *inner == Ty::STR,
            _ => false,
        }
    }

    /// Check an `@extern` declaration's annotation and signature; the C symbol, or `None`.
    pub(super) fn extern_symbol(&mut self, function: &ast::Function, signature: &Signature) -> Option<String> {
        let annotation = function.annotations.iter().find(|a| a.name.name == "extern")?;
        let symbol = match annotation.args.as_slice() {
            [ast::AnnotationArg { keyword: None, value: ast::AnnotationValue::Str(symbol), .. }] => symbol.clone(),
            _ => {
                self.error(Diagnostic::error(
                    codes::BAD_ANNOTATION,
                    "`@extern` takes the C function's name as a string",
                    annotation.span,
                    "for example `@extern(\"labs\")`",
                ));
                return None;
            }
        };
        if function.has_body {
            self.error(
                Diagnostic::error(
                    codes::BAD_ANNOTATION,
                    "an `@extern` function has no body",
                    function.name.span,
                    "remove the body",
                )
                .with_help("the declaration ends after the return type: `def labs(x: int) -> int`"),
            );
            return None;
        }
        if !function.generics.is_empty() || function.self_param.is_some() {
            self.error(Diagnostic::error(
                codes::BAD_ANNOTATION,
                "an `@extern` function cannot be generic or a method",
                function.name.span,
                "not allowed with `@extern`",
            ));
            return None;
        }
        let mut ok = true;
        for param in &signature.params {
            if !self.ffi_type(param.ty, false) {
                let shown = self.display(param.ty);
                self.error(
                    Diagnostic::error(
                        codes::BAD_ANNOTATION,
                        format!("a value of type `{shown}` cannot be passed to a C function"),
                        param.hint_span,
                        "not a C type",
                    )
                    .with_help("`@extern` functions take numbers, `bool` and `str`"),
                );
                ok = false;
            }
        }
        if !self.ffi_type(signature.ret, true) {
            let shown = self.display(signature.ret);
            self.error(
                Diagnostic::error(
                    codes::BAD_ANNOTATION,
                    format!("a C function cannot return a value of type `{shown}`"),
                    signature.ret_span.unwrap_or(signature.name_span),
                    "not a C type",
                )
                .with_help("`@extern` functions return numbers, `bool`, `str`, `str?` or nothing"),
            );
            ok = false;
        }
        ok.then_some(symbol)
    }

    pub(super) fn extern_function(&mut self, signature: &Signature, symbol: String) -> hir::Function {
        let locals: Vec<hir::Local> = signature
            .params
            .iter()
            .map(|p: &ParamSig| hir::Local { name: p.name.clone(), ty: p.ty, boxed: false, capture: None })
            .collect();
        hir::Function {
            name: signature.name.clone(),
            params: (0..locals.len()).map(LocalId).collect(),
            locals,
            ret: signature.ret,
            body: Vec::new(),
            generic: false,
            closure: false,
            extern_symbol: Some(symbol),
            span: None,
        }
    }

    /// A global used where a value is expected.
    pub(super) fn global_value(
        &mut self,
        cx: &mut FnCx,
        global: Global,
        name: &str,
        span: Span,
        expected: Option<Ty>,
    ) -> hir::Expr {
        match global {
            Global::Const(index) => self.const_value(index, name, span),
            Global::Function(id) => self.func_ref(id, span),
            Global::GenericFunction(index) => self.generic_fn_value(index, expected, span),
            Global::Missing => error_expr(span),
            Global::Module(_) => {
                self.error(
                    Diagnostic::error(
                        codes::NOT_SUPPORTED,
                        format!("`{name}` is a module, not a value"),
                        span,
                        "a module",
                    )
                    .with_help(format!("use one of its declarations: `{name}.something`")),
                );
                error_expr(span)
            }
            Global::Struct(_)
            | Global::Enum(_)
            | Global::Trait(_)
            | Global::GenericStruct(_)
            | Global::GenericEnum(_)
            | Global::TypeAlias(_) => {
                let _ = cx;
                let help = match global {
                    Global::Enum(_) | Global::GenericEnum(_) => {
                        format!("create a value with one of its variants: `{name}.Variant`")
                    }
                    Global::Trait(_) => {
                        "a trait describes behaviour; use a value of a type that implements it".to_string()
                    }
                    _ => format!("create a value with `{name}(...)`"),
                };
                self.error(
                    Diagnostic::error(codes::NOT_SUPPORTED, format!("`{name}` is a type, not a value"), span, "a type")
                        .with_help(help),
                );
                error_expr(span)
            }
        }
    }
}
