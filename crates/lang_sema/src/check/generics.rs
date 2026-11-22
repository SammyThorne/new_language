//! Generic functions, structs, enums and `impl` blocks, compiled by monomorphisation
//! (docs/02-type-system-and-traits.md, "Generics").
//!
//! Each generic declaration is a *template*. Its type parameters are [`TyKind::Param`] types,
//! and each has bounds: the traits a type argument must implement.
//!
//! - The template is checked once, with its own parameters as the type arguments (the
//!   *pattern* instance). Inside it, a parameter only supports what its bounds provide. Errors
//!   are reported from this check, and its code is not compiled.
//! - Each use with concrete types creates an *instance*: a new function, struct or enum with
//!   the parameters replaced, checked again quietly and compiled like hand-written code.
//! - Uses whose type arguments are still parameters (one generic function calling another)
//!   create instances that only have signatures; they are needed to check the caller.
//!
//! Type arguments are inferred from the arguments of a call, and from the type the context
//! expects; they can also be written: `Stack[int]()`, `Stack[int].new()`.

use super::{
    Body, BodyMode, Checker, EnumInfo, FnCx, Global, ImplInfo, KNOWN_TYPES, ParamSig, Reason, StructInfo, error_expr,
    plural,
};
use crate::hir::{self, FuncId};
use crate::types::{EnumId, TraitId, Ty, TyKind};
use lang_diag::{Diagnostic, Span, codes};
use lang_syntax::ast;
use std::collections::HashMap;

/// How deeply type arguments may nest, which stops runaway instantiation such as a function
/// `f[T]` that calls `f[list[T]]`.
const MAX_DEPTH: usize = 10;

#[derive(Clone, Debug)]
pub(super) struct TypeParam {
    pub name: String,
    pub span: Span,
    pub index: u32,
    pub ty: Ty,
}

pub(super) struct GenericFn<'m> {
    pub ast: &'m ast::Function,
    pub params: Vec<TypeParam>,
    pub instances: HashMap<Vec<Ty>, FuncId>,
    /// The instance with the function's own parameters, used for inference.
    pub pattern: Option<FuncId>,
    /// For a method with type parameters of its own: what it belongs to.
    pub method: Option<MethodOf>,
}

/// The `impl` block instance a generic method is in (docs/11, milestone 4): the type, its name
/// for messages, the bindings of the block's type parameters, and how the block's bodies are
/// checked.
#[derive(Clone)]
pub(super) struct MethodOf {
    pub target: Ty,
    pub type_name: String,
    pub bindings: Vec<(String, Ty)>,
    pub mode: BodyMode,
}

#[derive(Clone, Copy)]
pub(super) enum TypeDecl<'m> {
    Struct(&'m ast::StructDecl),
    Enum(&'m ast::EnumDecl),
}

pub(super) struct GenericType<'m> {
    pub decl: TypeDecl<'m>,
    pub name: String,
    pub span: Span,
    pub params: Vec<TypeParam>,
    pub instances: HashMap<Vec<Ty>, Ty>,
    /// `impl` templates for this type, by index into the checker's list.
    pub impls: Vec<usize>,
}

pub(super) struct ImplTemplate<'m> {
    pub ast: &'m ast::ImplDecl,
    pub params: Vec<TypeParam>,
    /// The target's type arguments, in terms of `params`: `[T]` in `impl[T] Stack[T]:`.
    pub pattern: Vec<Ty>,
}

/// Which argument fills each parameter of a call: an expression, or `None` for the default.
pub(super) type Arranged<'e> = Vec<Option<&'e ast::Expr>>;

/// What a prelude function that stands for a method of a built-in type has besides the written
/// arguments (docs/11, milestone 2): the name to show in messages, the receiver, already
/// checked, as its first argument, and the text of the call's location as its hidden last one.
#[derive(Default)]
pub(super) struct Extra {
    pub name: Option<String>,
    pub receiver: Option<hir::Expr>,
    /// Whether the receiver is the function's first parameter (a prelude function standing for
    /// a method of a built-in type), rather than the `self` of a method, which is not one.
    pub receiver_is_param: bool,
    pub location: Option<hir::Expr>,
}

impl Extra {
    /// The parameters the caller writes: all but the receiver and the location.
    pub fn written<'p>(&self, params: &'p [super::ParamSig]) -> &'p [super::ParamSig] {
        let first = usize::from(self.receiver.is_some() && self.receiver_is_param);
        let last = params.len() - usize::from(self.location.is_some());
        &params[first..last]
    }
}

impl<'m> Checker<'_, 'm> {
    // ---- declaring templates ------------------------------------------------------------

    /// Create the type parameters of a declaration. Bounds are filled in by [`Self::resolve_bounds`]
    /// once all traits are known.
    pub(super) fn declare_type_params(&mut self, generics: &[ast::GenericParam]) -> Vec<TypeParam> {
        let mut params: Vec<TypeParam> = Vec::new();
        for param in generics {
            if let Some(first) = params.iter().find(|p| p.name == param.name.name) {
                let first_span = first.span;
                self.error(
                    Diagnostic::error(
                        codes::DUPLICATE_DEFINITION,
                        format!("type parameter `{}` is listed twice", param.name.name),
                        param.name.span,
                        "listed again here",
                    )
                    .with_label(first_span, "first listed here"),
                );
                continue;
            }
            if KNOWN_TYPES.contains(&param.name.name.as_str()) {
                self.error(Diagnostic::error(
                    codes::DUPLICATE_DEFINITION,
                    format!("`{}` is a built-in type and cannot be a type parameter", param.name.name),
                    param.name.span,
                    "a built-in type",
                ));
                continue;
            }
            let (index, ty) = self.types.add_param(&param.name.name);
            self.params.push(Vec::new());
            params.push(TypeParam { name: param.name.name.clone(), span: param.name.span, index, ty });
        }
        params
    }

    pub(super) fn resolve_bounds(&mut self, params: &[TypeParam], generics: &[ast::GenericParam]) {
        for param in generics {
            let Some(declared) = params.iter().find(|p| p.name == param.name.name) else { continue };
            let index = declared.index as usize;
            for bound in &param.bounds {
                match self.globals.get(&bound.name) {
                    Some(&(Global::Trait(id), _)) => self.params[index].push(id),
                    _ => {
                        let mut diagnostic = Diagnostic::error(
                            codes::UNKNOWN_TYPE,
                            format!("unknown trait `{}`", bound.name),
                            bound.span,
                            "not a trait",
                        )
                        .with_help("a bound names a trait the type argument must implement, as in `T: Ord`");
                        if KNOWN_TYPES.contains(&bound.name.as_str()) {
                            diagnostic = diagnostic.with_help(format!(
                                "`{}` is a type; bounds are traits such as `Number`, `Ord` or `Display`",
                                bound.name
                            ));
                        }
                        self.error(diagnostic);
                    }
                }
            }
        }
    }

    pub(super) fn bindings(params: &[TypeParam], args: &[Ty]) -> Vec<(String, Ty)> {
        params.iter().zip(args).map(|(p, a)| (p.name.clone(), *a)).collect()
    }

    /// Run `body` with type parameter names bound to types, and with diagnostics dropped when
    /// `quiet`.
    pub(super) fn with_scope<R>(
        &mut self,
        bindings: Vec<(String, Ty)>,
        quiet: bool,
        body: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let saved_scope = std::mem::replace(&mut self.type_scope, bindings);
        let saved_quiet = self.diags.quiet;
        self.diags.quiet |= quiet;
        let result = body(self);
        self.diags.quiet = saved_quiet;
        self.type_scope = saved_scope;
        result
    }

    pub(super) fn type_param(&self, name: &str) -> Option<Ty> {
        self.type_scope.iter().rev().find(|(n, _)| n == name).map(|(_, t)| *t)
    }

    fn own_types(params: &[TypeParam]) -> Vec<Ty> {
        params.iter().map(|p| p.ty).collect()
    }

    /// How instances are checked: the pattern reports errors, instances with concrete types
    /// are checked quietly and compiled, and the rest only need signatures.
    fn instance_mode(&self, args: &[Ty], pattern: &[Ty]) -> BodyMode {
        if args == pattern {
            BodyMode::GenericCheck
        } else if args.iter().any(|a| self.types.contains_param(*a)) {
            BodyMode::Skip
        } else {
            BodyMode::Quiet
        }
    }

    fn depth(&self, ty: Ty) -> usize {
        let inner = match self.types.kind(ty) {
            TyKind::List(t)
            | TyKind::Option(t)
            | TyKind::FrozenList(t)
            | TyKind::Task(t)
            | TyKind::Channel(t)
            | TyKind::Mutex(t) => self.depth(*t),
            TyKind::FrozenDict(k, v) => self.depth(*k).max(self.depth(*v)),
            TyKind::Dict(k, v) | TyKind::Result(k, v) => self.depth(*k).max(self.depth(*v)),
            TyKind::Tuple(items) => items.iter().map(|t| self.depth(*t)).max().unwrap_or(0),
            TyKind::Func(params, ret) => params.iter().map(|t| self.depth(*t)).max().unwrap_or(0).max(self.depth(*ret)),
            TyKind::Struct(_) | TyKind::Enum(_) => match self.instance_of.get(&ty) {
                Some((_, args)) => args.iter().map(|t| self.depth(*t)).max().unwrap_or(0),
                None => 0,
            },
            _ => 0,
        };
        inner + 1
    }

    fn too_deep(&mut self, args: &[Ty], span: Span) -> bool {
        if args.iter().any(|a| self.depth(*a) > MAX_DEPTH) {
            self.error(
                Diagnostic::error(
                    codes::NOT_SUPPORTED,
                    "type arguments nest too deeply",
                    span,
                    "each use creates a more deeply nested type",
                )
                .with_help("a generic function that calls itself with a bigger type argument cannot be compiled"),
            );
            return true;
        }
        false
    }

    // ---- generic functions --------------------------------------------------------------

    /// Declare a generic function's name and type parameters; [`Self::finish_generic_fn`]
    /// resolves its bounds and signature once all traits and types are known.
    pub(super) fn declare_generic_fn(&mut self, function: &'m ast::Function) -> usize {
        let params = self.declare_type_params(&function.generics);
        let index = self.generic_fns.len();
        self.generic_fns.push(GenericFn {
            ast: function,
            params,
            instances: HashMap::new(),
            pattern: None,
            method: None,
        });
        self.globals.insert(function.name.name.clone(), (Global::GenericFunction(index), function.name.span));
        index
    }

    /// A method with type parameters of its own, in the `impl` block instance for `target`:
    /// a template like a generic function's, which keeps the block's bindings and `self`.
    pub(super) fn declare_generic_method(
        &mut self,
        function: &'m ast::Function,
        target: Ty,
        type_name: &str,
        bindings: &[(String, Ty)],
        mode: BodyMode,
    ) -> usize {
        // The method's own type parameters are made once, whichever instance of the `impl` block
        // it is in: otherwise `Box[U]`, named in `def map[U](...) -> Box[U]`, would be a new type
        // with a new `map` for every instance, without end.
        let key = (function.name.span.file, function.name.span.lo);
        let (params, fresh) = match self.method_params.get(&key) {
            Some(params) => (params.clone(), false),
            None => {
                let params = self.declare_type_params(&function.generics);
                self.method_params.insert(key, params.clone());
                (params, true)
            }
        };
        let index = self.generic_fns.len();
        self.generic_fns.push(GenericFn {
            ast: function,
            params,
            instances: HashMap::new(),
            pattern: None,
            method: Some(MethodOf { target, type_name: type_name.to_string(), bindings: bindings.to_vec(), mode }),
        });
        self.generic_methods.insert((target, function.name.name.clone()), index);
        self.finish_generic_fn_with(index, fresh);
        index
    }

    pub(super) fn finish_generic_fn(&mut self, index: usize) {
        self.finish_generic_fn_with(index, true);
    }

    fn finish_generic_fn_with(&mut self, index: usize, resolve: bool) {
        let (function, params) = (self.generic_fns[index].ast, self.generic_fns[index].params.clone());
        if resolve {
            self.resolve_bounds(&params, &function.generics);
        }
        if !function.annotations.is_empty() {
            let span = function.annotations[0].span;
            self.error(Diagnostic::error(
                codes::BAD_ANNOTATION,
                "a generic function cannot have annotations",
                span,
                "not allowed here",
            ));
        }
        let pattern = self.instantiate_fn(index, Self::own_types(&params), function.name.span);
        self.generic_fns[index].pattern = Some(pattern);
    }

    pub(super) fn instantiate_fn(&mut self, index: usize, args: Vec<Ty>, span: Span) -> FuncId {
        if let Some(&id) = self.generic_fns[index].instances.get(&args) {
            return id;
        }
        if self.too_deep(&args, span)
            && let Some(pattern) = self.generic_fns[index].pattern
        {
            return pattern;
        }
        let (ast, params) = (self.generic_fns[index].ast, self.generic_fns[index].params.clone());
        let method = self.generic_fns[index].method.clone();
        let own_mode = self.instance_mode(&args, &Self::own_types(&params));
        // A method's body is checked once, reported, in the pattern of its `impl` block, and its
        // own pattern in a concrete instance of the block has nothing to compile.
        let mode = match (&method, own_mode) {
            (None, mode) => mode,
            (Some(m), own) => match (m.mode, own) {
                (BodyMode::Skip, _) => BodyMode::Skip,
                (BodyMode::GenericCheck, BodyMode::GenericCheck) => BodyMode::GenericCheck,
                (BodyMode::GenericCheck, _) => BodyMode::Skip,
                (BodyMode::Quiet, BodyMode::GenericCheck) => BodyMode::Skip,
                (_, own) => own,
            },
        };
        let shown: Vec<String> = args.iter().map(|a| self.display(*a)).collect();
        let mut bindings = method.as_ref().map(|m| m.bindings.clone()).unwrap_or_default();
        bindings.extend(Self::bindings(&params, &args));
        let target = method.as_ref().map(|m| m.target);
        let saved_self = std::mem::replace(&mut self.self_ty, target);
        let signature = self.in_module(self.module_of(ast.name.span), |this| {
            let base = match &method {
                Some(m) => format!("{}.{}", m.type_name, ast.name.name),
                None => this.qualified(&ast.name.name),
            };
            let name = if mode == BodyMode::GenericCheck { base } else { format!("{base}[{}]", shown.join(", ")) };
            let receiver = target.zip(ast.self_param);
            this.with_scope(bindings.clone(), mode != BodyMode::GenericCheck, |this| {
                let mut signature = this.signature(ast, &name, receiver);
                signature.private = method.is_some() && ast.visibility == ast::Visibility::Priv;
                signature
            })
        });
        self.self_ty = saved_self;
        let id = FuncId(self.signatures.len());
        self.signatures.push(signature);
        self.bodies.push(Some(Body { ast, self_ty: target, bindings, mode, extern_symbol: None }));
        self.generic_fns[index].instances.insert(args, id);
        id
    }

    // ---- generic types ------------------------------------------------------------------

    pub(super) fn declare_generic_type(&mut self, decl: TypeDecl<'m>) {
        let (name, generics) = match decl {
            TypeDecl::Struct(d) => (&d.name, &d.generics),
            TypeDecl::Enum(d) => (&d.name, &d.generics),
        };
        let params = self.declare_type_params(generics);
        let index = self.generic_types.len();
        self.generic_types.push(GenericType {
            decl,
            name: name.name.clone(),
            span: name.span,
            params,
            instances: HashMap::new(),
            impls: Vec::new(),
        });
        let global = match decl {
            TypeDecl::Struct(_) => Global::GenericStruct(index),
            TypeDecl::Enum(d) => {
                for (variant_index, variant) in d.variants.iter().enumerate() {
                    self.generic_variant_names
                        .entry(variant.name.name.clone())
                        .or_default()
                        .push((index, variant_index));
                }
                Global::GenericEnum(index)
            }
        };
        self.globals.insert(name.name.clone(), (global, name.span));
    }

    /// Resolve the bounds of every generic type and create its pattern instance.
    pub(super) fn resolve_generic_types(&mut self) {
        for index in 0..self.generic_types.len() {
            let params = self.generic_types[index].params.clone();
            let generics = match self.generic_types[index].decl {
                TypeDecl::Struct(d) => &d.generics,
                TypeDecl::Enum(d) => &d.generics,
            };
            self.enter_module(self.module_of(self.generic_types[index].span));
            self.resolve_bounds(&params, generics);
        }
        for index in 0..self.generic_types.len() {
            let params = self.generic_types[index].params.clone();
            let span = self.generic_types[index].span;
            self.instantiate_type(index, Self::own_types(&params), span);
        }
    }

    pub(super) fn is_pattern_instance(&self, ty: Ty) -> bool {
        match self.instance_of.get(&ty) {
            Some((template, args)) => *args == Self::own_types(&self.generic_types[*template].params),
            None => false,
        }
    }

    /// The instance of generic type `index` for these type arguments, created on first use.
    pub(super) fn instantiate_type(&mut self, index: usize, args: Vec<Ty>, span: Span) -> Ty {
        if let Some(&ty) = self.generic_types[index].instances.get(&args) {
            return ty;
        }
        let module = self.module_of(self.generic_types[index].span);
        self.in_module(module, |this| this.instantiate_type_here(index, args, span))
    }

    fn instantiate_type_here(&mut self, index: usize, args: Vec<Ty>, span: Span) -> Ty {
        if self.too_deep(&args, span) {
            return Ty::ERROR;
        }
        let template = &self.generic_types[index];
        let (decl, name, decl_span, params) =
            (template.decl, template.name.clone(), template.span, template.params.clone());
        let shown = format!("{name}[{}]", args.iter().map(|a| self.display(*a)).collect::<Vec<_>>().join(", "));
        let generic = args.iter().any(|a| self.types.contains_param(*a));
        let is_pattern = args == Self::own_types(&params);
        let bindings = Self::bindings(&params, &args);
        let saved_self = self.self_ty.take();
        let ty = match decl {
            TypeDecl::Struct(d) => {
                let (id, ty) = self.types.add_struct(&shown);
                let frozen = d.annotations.iter().any(|a| a.name.name == "frozen");
                self.structs.push(StructInfo {
                    name: shown,
                    span: decl_span,
                    ty,
                    fields: Vec::new(),
                    frozen,
                    generic,
                    json_strict: false,
                });
                self.generic_types[index].instances.insert(args.clone(), ty);
                self.instance_of.insert(ty, (index, args.clone()));
                let fields = self.with_scope(bindings, !is_pattern, |this| this.fields(&d.fields));
                self.structs[id.0].fields = fields;
                ty
            }
            TypeDecl::Enum(d) => {
                let (id, ty) = self.types.add_enum(&shown);
                self.enums.push(EnumInfo { name: shown, span: decl_span, ty, variants: Vec::new(), generic });
                self.generic_types[index].instances.insert(args.clone(), ty);
                self.instance_of.insert(ty, (index, args.clone()));
                let variants = self.with_scope(bindings, !is_pattern, |this| this.variants(&d.variants, None));
                self.enums[id.0].variants = variants;
                ty
            }
        };
        self.self_ty = saved_self;
        if self.generic_setup_done {
            self.setup_instance(ty);
        } else {
            self.pending_instances.push(ty);
        }
        ty
    }

    /// Once every `impl` block is known: set up the instances created so far.
    pub(super) fn finish_generic_setup(&mut self) {
        self.generic_setup_done = true;
        for ty in std::mem::take(&mut self.pending_instances) {
            self.setup_instance(ty);
        }
    }

    /// Give an instance its field defaults, derived traits and the methods of every `impl`
    /// template that applies to it.
    fn setup_instance(&mut self, ty: Ty) {
        let Some((index, args)) = self.instance_of.get(&ty).cloned() else { return };
        let params = self.generic_types[index].params.clone();
        let is_pattern = args == Self::own_types(&params);
        let bindings = Self::bindings(&params, &args);
        let decl = self.generic_types[index].decl;

        if let (TypeDecl::Struct(d), Some(id)) = (decl, self.struct_of(ty)) {
            let module = self.module_of(d.name.span);
            self.in_module(module, |this| {
                this.with_scope(bindings.clone(), !is_pattern, |this| this.field_defaults(id, d))
            });
        }

        let annotations = match decl {
            TypeDecl::Struct(d) => &d.annotations,
            TypeDecl::Enum(d) => &d.annotations,
        };
        for annotation in annotations.iter().filter(|a| a.name.name == "derive") {
            let names = self.with_scope(Vec::new(), !is_pattern, |this| this.annotation_names(annotation));
            for arg in names {
                // Unknown names are reported once, for the pattern.
                let traits = self.with_scope(Vec::new(), !is_pattern, |this| this.derivable(arg));
                for trait_id in traits {
                    // An instance gets a derived trait when every type argument has it, as with a
                    // bound on each parameter. The pattern always gets it, for generic code.
                    if is_pattern || args.iter().all(|a| self.supports(*a, trait_id)) {
                        let methods = vec![None; self.traits[trait_id.0].methods.len()];
                        let span = arg.span;
                        self.with_scope(Vec::new(), true, |this| {
                            this.add_impl(ImplInfo { trait_id, ty, span, methods, derived: true })
                        });
                    }
                }
            }
        }

        for template in self.generic_types[index].impls.clone() {
            let module = self.module_of(self.impl_templates[template].ast.target.span);
            self.in_module(module, |this| this.apply_impl_template(template, ty, &args));
        }
    }

    /// Whether a type has a trait, counting the built-in implementations of `int`, `str` and
    /// the other built-in types.
    pub(super) fn supports(&self, ty: Ty, trait_id: TraitId) -> bool {
        let b = self.builtin();
        if [b.to_json, b.from_json, b.shareable].contains(&trait_id) {
            return self.implements(ty, trait_id);
        }
        match self.types.kind(ty) {
            TyKind::Struct(_) | TyKind::Enum(_) | TyKind::Dyn(_) | TyKind::SelfType(_) | TyKind::Param(_) => {
                self.implements(ty, trait_id)
            }
            _ if trait_id == b.display => self.displayable(ty),
            _ if trait_id == b.eq => self.equatable(ty),
            _ if trait_id == b.hash => self.hashable(ty),
            _ if trait_id == b.ord => self.orderable(ty),
            _ if trait_id == b.copy => !matches!(self.types.kind(ty), TyKind::Func(..)),
            _ if [b.add, b.sub, b.mul, b.number].contains(&trait_id) => {
                matches!(self.types.kind(ty), TyKind::Int | TyKind::Float | TyKind::Error)
            }
            TyKind::Error => true,
            _ => false,
        }
    }

    pub(super) fn declare_impl_template(&mut self, imp: &'m ast::ImplDecl, target: usize) {
        let params = self.declare_type_params(&imp.generics);
        self.resolve_bounds(&params, &imp.generics);
        let template_params = self.generic_types[target].params.len();
        let name = self.generic_types[target].name.clone();
        if imp.target_args.len() != template_params {
            self.error(
                Diagnostic::error(
                    codes::UNEXPECTED_TYPE_ARGS,
                    format!("`{name}` takes {}", plural(template_params, "type argument")),
                    imp.target.span,
                    "wrong number of type arguments",
                )
                .with_help(format!("write the type parameters after `impl` and pass them on: `impl[T] {name}[T]:`")),
            );
            return;
        }
        let bindings = Self::bindings(&params, &Self::own_types(&params));
        let pattern: Vec<Ty> = self
            .with_scope(bindings, false, |this| imp.target_args.iter().map(|a| this.resolve_value_type(a)).collect());
        if pattern.contains(&Ty::ERROR) {
            return;
        }
        for param in &params {
            if !pattern.iter().any(|t| self.mentions_param(*t, param.index)) {
                self.error(Diagnostic::error(
                    codes::UNEXPECTED_TYPE_ARGS,
                    format!("type parameter `{}` is not used in `{name}[...]`", param.name),
                    param.span,
                    "unused",
                ));
                return;
            }
        }
        let template = self.impl_templates.len();
        self.impl_templates.push(ImplTemplate { ast: imp, params, pattern: pattern.clone() });
        self.generic_types[target].impls.push(template);
        // The instance that checks the `impl` block's methods against the bounds.
        self.instantiate_type(target, pattern, imp.target.span);
    }

    fn mentions_param(&self, ty: Ty, index: u32) -> bool {
        match self.types.kind(ty) {
            TyKind::Param(i) => *i == index,
            TyKind::List(t)
            | TyKind::Option(t)
            | TyKind::FrozenList(t)
            | TyKind::Task(t)
            | TyKind::Channel(t)
            | TyKind::Mutex(t) => self.mentions_param(*t, index),
            TyKind::FrozenDict(k, v) => self.mentions_param(*k, index) || self.mentions_param(*v, index),
            TyKind::Dict(k, v) | TyKind::Result(k, v) => {
                self.mentions_param(*k, index) || self.mentions_param(*v, index)
            }
            TyKind::Tuple(items) => items.iter().any(|t| self.mentions_param(*t, index)),
            TyKind::Func(params, ret) => {
                params.iter().any(|t| self.mentions_param(*t, index)) || self.mentions_param(*ret, index)
            }
            TyKind::Struct(_) | TyKind::Enum(_) => match self.instance_of.get(&ty) {
                Some((_, args)) => args.iter().any(|t| self.mentions_param(*t, index)),
                None => false,
            },
            _ => false,
        }
    }

    fn apply_impl_template(&mut self, template: usize, ty: Ty, args: &[Ty]) {
        let (imp, params, pattern) = {
            let t = &self.impl_templates[template];
            (t.ast, t.params.clone(), t.pattern.clone())
        };
        let mut found = vec![None; params.len()];
        for (p, a) in pattern.iter().zip(args) {
            if !self.unify(*p, *a, &params, &mut found) {
                return;
            }
        }
        let Some(bound) = found.into_iter().collect::<Option<Vec<Ty>>>() else { return };
        for (param, arg) in params.iter().zip(&bound) {
            for required in self.params[param.index as usize].clone() {
                if !self.supports(*arg, required) {
                    return;
                }
            }
        }
        let mode = self.instance_mode(args, &pattern);
        let bindings = Self::bindings(&params, &bound);
        let type_name = self.display(ty);
        let quiet = mode != BodyMode::GenericCheck;
        match &imp.trait_name {
            None => self.with_scope(bindings.clone(), quiet, |this| {
                this.collect_inherent(imp, ty, &type_name, &bindings, mode)
            }),
            Some(trait_name) => match self.trait_named(&trait_name.name, trait_name.span) {
                Some(Global::Trait(trait_id)) => self.with_scope(bindings.clone(), quiet, |this| {
                    this.collect_trait_impl(imp, trait_id, ty, &type_name, &bindings, mode)
                }),
                _ => {
                    if !quiet {
                        self.error(Diagnostic::error(
                            codes::UNKNOWN_TYPE,
                            format!("unknown trait `{}`", trait_name.name),
                            trait_name.span,
                            "not a trait",
                        ));
                    }
                }
            },
        }
    }

    // ---- inference ----------------------------------------------------------------------

    /// Match `pattern` (mentioning `params`) against `actual`, recording what each parameter
    /// stands for. False when they cannot match. The error type matches anything.
    pub(super) fn unify(&self, pattern: Ty, actual: Ty, params: &[TypeParam], found: &mut [Option<Ty>]) -> bool {
        if actual == Ty::ERROR || pattern == Ty::ERROR {
            return true;
        }
        let pk = self.types.kind(pattern).clone();
        if let TyKind::Param(index) = pk
            && let Some(position) = params.iter().position(|p| p.index == index)
        {
            if actual == Ty::NONE {
                return true;
            }
            return match found[position] {
                None => {
                    found[position] = Some(actual);
                    true
                }
                Some(bound) => bound == actual || self.types.contains_error(actual) || self.types.contains_error(bound),
            };
        }
        let ak = self.types.kind(actual).clone();
        match (pk, ak) {
            (TyKind::List(p), TyKind::List(a))
            | (TyKind::FrozenList(p), TyKind::FrozenList(a))
            | (TyKind::Task(p), TyKind::Task(a))
            | (TyKind::Channel(p), TyKind::Channel(a))
            | (TyKind::Mutex(p), TyKind::Mutex(a)) => self.unify(p, a, params, found),
            (TyKind::FrozenDict(pk, pv), TyKind::FrozenDict(ak, av)) => {
                self.unify(pk, ak, params, found) && self.unify(pv, av, params, found)
            }
            (TyKind::Option(p), TyKind::Option(a)) => self.unify(p, a, params, found),
            (TyKind::Option(_), TyKind::None) => true,
            (TyKind::Option(p), _) => self.unify(p, actual, params, found),
            (TyKind::Dict(pk, pv), TyKind::Dict(ak, av)) | (TyKind::Result(pk, pv), TyKind::Result(ak, av)) => {
                self.unify(pk, ak, params, found) && self.unify(pv, av, params, found)
            }
            (TyKind::Result(p, _), _) => self.unify(p, actual, params, found),
            (TyKind::Tuple(ps), TyKind::Tuple(as_)) if ps.len() == as_.len() => {
                ps.iter().zip(&as_).all(|(p, a)| self.unify(*p, *a, params, found))
            }
            (TyKind::Func(pp, pr), TyKind::Func(ap, ar)) if pp.len() == ap.len() => {
                pp.iter().zip(&ap).all(|(p, a)| self.unify(*p, *a, params, found)) && self.unify(pr, ar, params, found)
            }
            (TyKind::Struct(_) | TyKind::Enum(_), TyKind::Struct(_) | TyKind::Enum(_)) => {
                match (self.instance_of.get(&pattern), self.instance_of.get(&actual)) {
                    (Some((pt, pargs)), Some((at, aargs))) if pt == at => {
                        pargs.clone().iter().zip(aargs.clone()).all(|(p, a)| self.unify(*p, a, params, found))
                    }
                    _ => pattern == actual,
                }
            }
            (TyKind::Dyn(_), _) => pattern == actual || self.conversion(actual, pattern).is_some(),
            _ => pattern == actual,
        }
    }

    /// Replace the parameters in `ty` with their types, creating instances of generic types as
    /// needed. `None` when some parameter in `ty` has no type yet.
    pub(super) fn subst(&mut self, ty: Ty, params: &[TypeParam], found: &[Option<Ty>]) -> Option<Ty> {
        Some(match self.types.kind(ty).clone() {
            TyKind::Param(index) => match params.iter().position(|p| p.index == index) {
                Some(position) => found[position]?,
                None => ty,
            },
            TyKind::List(t) => {
                let t = self.subst(t, params, found)?;
                self.types.list(t)
            }
            TyKind::Option(t) => {
                let t = self.subst(t, params, found)?;
                self.types.option(t)
            }
            TyKind::FrozenList(t) => {
                let t = self.subst(t, params, found)?;
                self.types.frozen_list(t)
            }
            TyKind::FrozenDict(k, v) => {
                let (k, v) = (self.subst(k, params, found)?, self.subst(v, params, found)?);
                self.types.frozen_dict(k, v)
            }
            TyKind::Task(t) => {
                let t = self.subst(t, params, found)?;
                self.types.intern(TyKind::Task(t))
            }
            TyKind::Channel(t) => {
                let t = self.subst(t, params, found)?;
                self.types.intern(TyKind::Channel(t))
            }
            TyKind::Mutex(t) => {
                let t = self.subst(t, params, found)?;
                self.types.intern(TyKind::Mutex(t))
            }
            TyKind::Dict(k, v) => {
                let (k, v) = (self.subst(k, params, found)?, self.subst(v, params, found)?);
                self.types.dict(k, v)
            }
            TyKind::Result(k, v) => {
                let (k, v) = (self.subst(k, params, found)?, self.subst(v, params, found)?);
                self.types.result(k, v)
            }
            TyKind::Tuple(items) => {
                let items = items.into_iter().map(|t| self.subst(t, params, found)).collect::<Option<Vec<_>>>()?;
                self.types.tuple(items)
            }
            TyKind::Func(ps, r) => {
                let ps = ps.into_iter().map(|t| self.subst(t, params, found)).collect::<Option<Vec<_>>>()?;
                let r = self.subst(r, params, found)?;
                self.types.func(ps, r)
            }
            TyKind::Struct(_) | TyKind::Enum(_) => match self.instance_of.get(&ty).cloned() {
                Some((template, args)) => {
                    let new_args = args.iter().map(|t| self.subst(*t, params, found)).collect::<Option<Vec<_>>>()?;
                    if new_args == args {
                        ty
                    } else {
                        let span = self.generic_types[template].span;
                        self.instantiate_type(template, new_args, span)
                    }
                }
                None => ty,
            },
            _ => ty,
        })
    }

    /// Check that each type argument has the traits its parameter requires.
    pub(super) fn check_bounds(&mut self, owner: &str, params: &[TypeParam], args: &[Ty], span: Span) -> bool {
        let mut ok = true;
        for (param, arg) in params.iter().zip(args) {
            for required in self.params[param.index as usize].clone() {
                if !self.supports(*arg, required) {
                    let (shown, tr) = (self.display(*arg), self.traits[required.0].name.clone());
                    let mut diagnostic = Diagnostic::error(
                        codes::UNSATISFIED_BOUND,
                        format!("`{shown}` does not implement `{tr}`, which `{owner}` requires of `{}`", param.name),
                        span,
                        format!("`{}` is `{shown}` here", param.name),
                    );
                    if param.span.file != self.prelude_file {
                        diagnostic =
                            diagnostic.with_label(param.span, format!("`{}: {tr}` is required here", param.name));
                    }
                    let json = [self.builtin().to_json, self.builtin().from_json].contains(&required);
                    if json && matches!(self.types.kind(*arg), TyKind::Struct(_) | TyKind::Enum(_)) {
                        diagnostic = diagnostic.with_help(format!("add `@derive(Json)` to `{shown}`"));
                    } else if matches!(self.types.kind(*arg), TyKind::Struct(_) | TyKind::Enum(_)) {
                        diagnostic = diagnostic.with_help(format!("implement `{tr}` for `{shown}`, or derive it"));
                    } else if let TyKind::Param(_) = self.types.kind(*arg) {
                        diagnostic =
                            diagnostic.with_help(format!("add the bound to the type parameter: `{shown}: {tr}`"));
                    }
                    self.error(diagnostic);
                    ok = false;
                }
            }
        }
        ok
    }

    fn cannot_infer(
        &mut self,
        owner: &str,
        params: &[TypeParam],
        found: &[Option<Ty>],
        span: Span,
        example: Option<&str>,
    ) {
        let missing: Vec<String> =
            params.iter().zip(found).filter(|(_, f)| f.is_none()).map(|(p, _)| format!("`{}`", p.name)).collect();
        self.error(
            Diagnostic::error(
                codes::CANNOT_INFER,
                format!("cannot infer the type parameter {} of `{owner}`", missing.join(" and ")),
                span,
                "type unknown",
            )
            .with_help(match example {
                Some(example) => format!(
                    "give the types explicitly, for example `{example}`, or add a type hint where the value is used"
                ),
                None => "add a type hint where the value is used, or give an argument that has the type".to_string(),
            }),
        );
    }

    /// Match the arguments of a call to the parameters: positional first, then by name, with
    /// defaults for the rest. Reports and returns `None` on a mismatch.
    pub(super) fn arrange<'e>(
        &mut self,
        name: &str,
        def_span: Span,
        params: &[ParamSig],
        args: &'e [ast::Arg],
        span: Span,
    ) -> Option<Arranged<'e>> {
        let mut slots: Vec<Option<&'e ast::Expr>> = vec![None; params.len()];
        let mut positional = 0;
        let mut seen_keyword = false;
        let mut ok = true;
        let uses_keywords = args.iter().any(|a| a.name.is_some());
        let has_defaults = params.iter().any(|p| p.default.is_some());
        // The prelude's source is not something a user reads, so it gets no "defined here".
        let in_prelude = def_span.file == self.prelude_file;
        let count_error = |this: &mut Self| {
            let given = args.len();
            let verb = if given == 1 { "was" } else { "were" };
            let mut diagnostic = Diagnostic::error(
                codes::WRONG_ARG_COUNT,
                format!("`{name}` takes {} but {given} {verb} given", plural(params.len(), "argument")),
                span,
                format!("expected {}", plural(params.len(), "argument")),
            );
            if !in_prelude {
                diagnostic = diagnostic.with_label(def_span, format!("`{name}` is defined here"));
            }
            this.error(diagnostic);
        };
        if !uses_keywords && !has_defaults && args.len() != params.len() {
            count_error(self);
            return None;
        }
        for arg in args {
            match &arg.name {
                None => {
                    if seen_keyword {
                        self.error(Diagnostic::error(
                            codes::UNEXPECTED_TOKEN,
                            "a positional argument cannot follow a keyword argument",
                            arg.value.span,
                            "positional",
                        ));
                        ok = false;
                        continue;
                    }
                    if positional >= params.len() {
                        count_error(self);
                        return None;
                    }
                    slots[positional] = Some(&arg.value);
                    positional += 1;
                }
                Some(keyword) => {
                    seen_keyword = true;
                    let Some(index) = params.iter().position(|p| p.name == keyword.name) else {
                        let mut diagnostic = Diagnostic::error(
                            codes::NO_SUCH_MEMBER,
                            format!("`{name}` has no parameter named `{}`", keyword.name),
                            keyword.span,
                            "no such parameter",
                        );
                        if !in_prelude {
                            diagnostic = diagnostic.with_label(def_span, format!("`{name}` is defined here"));
                        }
                        if let Some(s) = super::closest(&keyword.name, params.iter().map(|p| p.name.as_str())) {
                            diagnostic = diagnostic.with_help(format!("did you mean `{s}`?"));
                        }
                        self.error(diagnostic);
                        ok = false;
                        continue;
                    };
                    if slots[index].is_some() {
                        self.error(Diagnostic::error(
                            codes::DUPLICATE_DEFINITION,
                            format!("argument `{}` is given more than once", keyword.name),
                            keyword.span,
                            "given again here",
                        ));
                        ok = false;
                        continue;
                    }
                    slots[index] = Some(&arg.value);
                }
            }
        }
        let missing: Vec<String> = params
            .iter()
            .zip(&slots)
            .filter(|(p, s)| s.is_none() && p.default.is_none())
            .map(|(p, _)| format!("`{}`", p.name))
            .collect();
        if !missing.is_empty() {
            let word = if missing.len() == 1 { "argument" } else { "arguments" };
            let mut diagnostic = Diagnostic::error(
                codes::WRONG_ARG_COUNT,
                format!("`{name}` is missing the {word} {}", missing.join(", ")),
                span,
                format!("missing {}", missing.join(", ")),
            );
            if !in_prelude {
                diagnostic = diagnostic.with_label(def_span, format!("`{name}` is defined here"));
            }
            self.error(diagnostic);
            return None;
        }
        ok.then_some(slots)
    }

    /// Check arguments without reporting anything, to learn their types.
    fn speculate<R>(&mut self, body: impl FnOnce(&mut Self) -> R) -> R {
        let saved = self.diags.speculative;
        self.diags.speculative = true;
        let result = body(self);
        self.diags.speculative = saved;
        result
    }

    /// Infer type parameters from argument expressions, each matched with the type of the
    /// parameter or field it fills. Lambdas are checked once the types of their parameters are
    /// known, so `map(items, lambda x: x * 2)` learns `T` from `items` first.
    fn infer_from_args(
        &mut self,
        cx: &mut FnCx,
        slots: &[(Ty, &ast::Expr)],
        params: &[TypeParam],
        found: &mut [Option<Ty>],
    ) {
        self.speculate(|this| {
            let mut done = vec![false; slots.len()];
            loop {
                let mut progress = false;
                for (index, (pattern, expr)) in slots.iter().enumerate() {
                    if done[index] {
                        continue;
                    }
                    let builtin_name = match &expr.kind {
                        ast::ExprKind::Name(name)
                            if super::BUILTIN_FUNCTIONS.contains(&name.as_str()) && !cx.sees(name) =>
                        {
                            Some(name.clone())
                        }
                        _ => None,
                    };
                    if let Some(name) = builtin_name {
                        // A built-in function as a value needs its parameter types first.
                        let TyKind::Func(fps, _) = this.types.kind(*pattern).clone() else {
                            done[index] = true;
                            continue;
                        };
                        let Some(param_tys) =
                            fps.iter().map(|t| this.subst(*t, params, found)).collect::<Option<Vec<Ty>>>()
                        else {
                            continue;
                        };
                        let value = this.builtin_value(cx, &name, param_tys, expr.span);
                        this.unify(*pattern, value.ty, params, found);
                    } else if let ast::ExprKind::Lambda { params: lambda_params, body } = &expr.kind {
                        let TyKind::Func(fps, fr) = this.types.kind(*pattern).clone() else {
                            done[index] = true;
                            continue;
                        };
                        let Some(param_tys) =
                            fps.iter().map(|t| this.subst(*t, params, found)).collect::<Option<Vec<Ty>>>()
                        else {
                            continue;
                        };
                        if param_tys.len() != lambda_params.len() {
                            done[index] = true;
                            continue;
                        }
                        let ret = this.subst(fr, params, found);
                        let value = this.lambda_with(cx, lambda_params, body, param_tys, ret, expr.span);
                        this.unify(*pattern, value.ty, params, found);
                    } else {
                        let expected = this.subst(*pattern, params, found);
                        let value = this.expr(cx, expr, expected);
                        this.unify(*pattern, value.ty, params, found);
                    }
                    done[index] = true;
                    progress = true;
                }
                if !progress {
                    break;
                }
            }
        });
    }

    /// A call to a generic function: infer its type arguments, instantiate it, and check the
    /// call against the instance.
    pub(super) fn call_generic(
        &mut self,
        cx: &mut FnCx,
        index: usize,
        args: &[ast::Arg],
        span: Span,
        expected: Option<Ty>,
    ) -> hir::Expr {
        self.call_generic_with(cx, index, Extra::default(), args, span, expected)
    }

    /// `call_generic`, for a prelude function that stands for a method of a built-in type: the
    /// receiver is already checked and is the first parameter, and a hidden last parameter may
    /// take the call's location.
    pub(super) fn call_generic_with(
        &mut self,
        cx: &mut FnCx,
        index: usize,
        extra: Extra,
        args: &[ast::Arg],
        span: Span,
        expected: Option<Ty>,
    ) -> hir::Expr {
        let pattern = self.generic_fns[index].pattern.expect("generic functions have a pattern");
        let params = self.generic_fns[index].params.clone();
        let signature = self.signatures[pattern.0].clone();
        let name = extra.name.clone().unwrap_or_else(|| signature.name.clone());
        let written = extra.written(&signature.params);
        let Some(slots) = self.arrange(&name, signature.name_span, written, args, span) else {
            self.check_all_quietly(cx, args);
            return error_expr(span);
        };
        let pairs: Vec<(Ty, &ast::Expr)> =
            slots.iter().zip(written).filter_map(|(s, p)| s.map(|e| (p.ty, e))).collect();
        let mut found = vec![None; params.len()];
        if let Some(receiver) = extra.receiver.as_ref().filter(|_| extra.receiver_is_param) {
            self.unify(signature.params[0].ty, receiver.ty, &params, &mut found);
        }
        self.infer_from_args(cx, &pairs, &params, &mut found);
        if found.iter().any(Option::is_none)
            && let Some(expected) = expected
        {
            self.unify(signature.ret, expected, &params, &mut found);
        }
        let Some(bound) = found.iter().copied().collect::<Option<Vec<Ty>>>() else {
            // A method's type parameters cannot be written out, only inferred.
            let example = format!("{name}[{}](...)", params.iter().map(|_| "int").collect::<Vec<_>>().join(", "));
            let is_method =
                extra.name.is_some() && !extra.receiver_is_param && self.generic_fns[index].method.is_some();
            self.cannot_infer(&name, &params, &found, span, (!is_method).then_some(example.as_str()));
            self.check_all_quietly(cx, args);
            return error_expr(span);
        };
        self.finish_generic_call(cx, index, bound, extra, args, span)
    }

    /// `f[int, str](...)`: a generic function with explicit type arguments.
    pub(super) fn call_generic_explicit(
        &mut self,
        cx: &mut FnCx,
        index: usize,
        type_args: Vec<Ty>,
        args: &[ast::Arg],
        span: Span,
    ) -> hir::Expr {
        let params = self.generic_fns[index].params.clone();
        if type_args.len() != params.len() {
            let name = self.generic_fns[index].ast.name.name.clone();
            self.error(Diagnostic::error(
                codes::UNEXPECTED_TYPE_ARGS,
                format!("`{name}` takes {}", plural(params.len(), "type argument")),
                span,
                "wrong number of type arguments",
            ));
            self.check_all_quietly(cx, args);
            return error_expr(span);
        }
        self.finish_generic_call(cx, index, type_args, Extra::default(), args, span)
    }

    fn finish_generic_call(
        &mut self,
        cx: &mut FnCx,
        index: usize,
        bound: Vec<Ty>,
        extra: Extra,
        args: &[ast::Arg],
        span: Span,
    ) -> hir::Expr {
        let params = self.generic_fns[index].params.clone();
        let name = extra.name.clone().unwrap_or_else(|| self.generic_fns[index].ast.name.name.clone());
        if bound.contains(&Ty::ERROR) || !self.check_bounds(&name, &params, &bound, span) {
            self.check_all_quietly(cx, args);
            return error_expr(span);
        }
        let func = self.instantiate_fn(index, bound, span);
        let signature = self.signatures[func.0].clone();
        let written = extra.written(&signature.params);
        let mut values: Vec<hir::Expr> = extra.receiver.iter().cloned().collect();
        values.extend(self.call_args(cx, &name, signature.name_span, written, args, span));
        values.extend(extra.location);
        self.check_jobs_call(cx, func, &values);
        hir::Expr { kind: hir::ExprKind::Call { func, args: values }, ty: signature.ret, span }
    }

    /// A generic function used as a value: its type arguments come from the expected function
    /// type.
    pub(super) fn generic_fn_value(&mut self, index: usize, expected: Option<Ty>, span: Span) -> hir::Expr {
        let pattern = self.generic_fns[index].pattern.expect("generic functions have a pattern");
        let params = self.generic_fns[index].params.clone();
        let name = self.generic_fns[index].ast.name.name.clone();
        let signature = self.signatures[pattern.0].clone();
        let mut found = vec![None; params.len()];
        if let Some(expected) = expected
            && let TyKind::Func(eps, er) = self.types.kind(expected).clone()
            && eps.len() == signature.params.len()
        {
            for (p, e) in signature.params.iter().zip(&eps) {
                self.unify(p.ty, *e, &params, &mut found);
            }
            self.unify(signature.ret, er, &params, &mut found);
        }
        let Some(bound) = found.iter().copied().collect::<Option<Vec<Ty>>>() else {
            self.cannot_infer(&name, &params, &found, span, Some(&format!("{name}[int]")));
            return error_expr(span);
        };
        if !self.check_bounds(&name, &params, &bound, span) {
            return error_expr(span);
        }
        let func = self.instantiate_fn(index, bound, span);
        self.func_ref(func, span)
    }

    pub(super) fn check_all_quietly(&mut self, cx: &mut FnCx, args: &[ast::Arg]) {
        self.speculate(|this| {
            for arg in args {
                this.expr(cx, &arg.value, None);
            }
        });
    }

    // ---- generic structs and enums in expressions --------------------------------------

    /// Type arguments written in an expression, as in `Stack[int]` or `Pair[str, int]`: the
    /// index part of an indexing expression.
    pub(super) fn type_args_of(&mut self, index: &ast::Expr) -> Option<Vec<Ty>> {
        let items: Vec<&ast::Expr> = match &index.kind {
            ast::ExprKind::Tuple(items) => items.iter().collect(),
            _ => vec![index],
        };
        let mut out = Vec::new();
        for item in items {
            let Some(hint) = expr_to_type(item) else {
                self.error(Diagnostic::error(codes::UNKNOWN_TYPE, "expected a type argument", item.span, "not a type"));
                return None;
            };
            let ty = self.resolve_value_type(&hint);
            if ty == Ty::ERROR {
                return None;
            }
            out.push(ty);
        }
        Some(out)
    }

    /// The instance a generic type expression names, such as `Stack[int]`, after checking the
    /// number of arguments and the bounds.
    pub(super) fn explicit_instance(&mut self, template: usize, index: &ast::Expr, span: Span) -> Option<Ty> {
        let args = self.type_args_of(index)?;
        let params = self.generic_types[template].params.clone();
        let name = self.generic_types[template].name.clone();
        if args.len() != params.len() {
            self.error(Diagnostic::error(
                codes::UNEXPECTED_TYPE_ARGS,
                format!("`{name}` takes {}", plural(params.len(), "type argument")),
                span,
                "wrong number of type arguments",
            ));
            return None;
        }
        if !self.check_bounds(&name, &params, &args, span) {
            return None;
        }
        Some(self.instantiate_type(template, args, span))
    }

    /// The type arguments of `expected` if it is an instance of `template`.
    fn expected_instance(&self, template: usize, expected: Option<Ty>) -> Option<Ty> {
        let expected = expected?;
        let expected = self.types.option_inner(expected).unwrap_or(expected);
        match self.instance_of.get(&expected) {
            Some((t, _)) if *t == template => Some(expected),
            _ => None,
        }
    }

    /// Infer a generic type's arguments from values given for its fields (`fields` are the
    /// pattern instance's), then instantiate it.
    #[allow(clippy::too_many_arguments)]
    fn infer_instance(
        &mut self,
        cx: &mut FnCx,
        template: usize,
        fields: &[super::FieldInfo],
        args: &[ast::Arg],
        expected: Option<Ty>,
        span: Span,
        example: &str,
    ) -> Option<Ty> {
        if let Some(ty) = self.expected_instance(template, expected) {
            return Some(ty);
        }
        let params = self.generic_types[template].params.clone();
        let name = self.generic_types[template].name.clone();
        let mut pairs: Vec<(Ty, &ast::Expr)> = Vec::new();
        for (position, arg) in args.iter().enumerate() {
            let field = match &arg.name {
                Some(keyword) => fields.iter().find(|f| f.name == keyword.name),
                None => fields.get(position),
            };
            if let Some(field) = field {
                pairs.push((field.ty, &arg.value));
            }
        }
        let mut found = vec![None; params.len()];
        self.infer_from_args(cx, &pairs, &params, &mut found);
        let Some(bound) = found.iter().copied().collect::<Option<Vec<Ty>>>() else {
            self.cannot_infer(&name, &params, &found, span, Some(example));
            return None;
        };
        if bound.contains(&Ty::ERROR) || !self.check_bounds(&name, &params, &bound, span) {
            return None;
        }
        Some(self.instantiate_type(template, bound, span))
    }

    /// `Box(value=1)`, or `Box[int](...)` when `instance` is given.
    pub(super) fn construct_generic(
        &mut self,
        cx: &mut FnCx,
        template: usize,
        instance: Option<Ty>,
        args: &[ast::Arg],
        span: Span,
        expected: Option<Ty>,
    ) -> hir::Expr {
        let ty = match instance {
            Some(ty) => Some(ty),
            None => {
                let pattern_ty = self.pattern_of(template);
                let fields = self.struct_of(pattern_ty).map(|id| self.structs[id.0].fields.clone()).unwrap_or_default();
                let name = self.generic_types[template].name.clone();
                self.infer_instance(cx, template, &fields, args, expected, span, &format!("{name}[int](...)"))
            }
        };
        match ty.and_then(|t| self.struct_of(t)) {
            Some(id) => self.construct(cx, id, args, span),
            None => {
                self.check_all_quietly(cx, args);
                error_expr(span)
            }
        }
    }

    pub(super) fn pattern_of(&self, template: usize) -> Ty {
        let args = Self::own_types(&self.generic_types[template].params);
        self.generic_types[template].instances[&args]
    }

    pub(super) fn enum_of(&self, ty: Ty) -> Option<EnumId> {
        match self.types.kind(ty) {
            TyKind::Enum(id) => Some(*id),
            _ => None,
        }
    }

    /// A variant of a generic enum: `Leaf`, `Tree.Node(...)`, `Tree[int].Leaf`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn generic_variant(
        &mut self,
        cx: &mut FnCx,
        template: usize,
        variant: usize,
        instance: Option<Ty>,
        args: Option<&[ast::Arg]>,
        span: Span,
        expected: Option<Ty>,
    ) -> hir::Expr {
        let name = self.generic_types[template].name.clone();
        let ty = match instance {
            Some(ty) => Some(ty),
            None => {
                let pattern_ty = self.pattern_of(template);
                let fields = self
                    .enum_of(pattern_ty)
                    .map(|id| self.enums[id.0].variants[variant].fields.clone())
                    .unwrap_or_default();
                let variant_name = self.enum_of(pattern_ty).map(|id| self.enums[id.0].variants[variant].name.clone());
                let example = format!("{name}[int].{}", variant_name.unwrap_or_default());
                self.infer_instance(cx, template, &fields, args.unwrap_or_default(), expected, span, &example)
            }
        };
        match ty.and_then(|t| self.enum_of(t)) {
            Some(id) => self.variant_value(cx, id, variant, args, span),
            None => {
                if let Some(args) = args {
                    self.check_all_quietly(cx, args);
                }
                error_expr(span)
            }
        }
    }

    /// `Stack[int].new()`, or `Stack.new()` where the expected type says which instance.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn generic_static_call(
        &mut self,
        cx: &mut FnCx,
        template: usize,
        instance: Option<Ty>,
        name: &ast::Ident,
        args: &[ast::Arg],
        span: Span,
        expected: Option<Ty>,
    ) -> hir::Expr {
        let type_name = self.generic_types[template].name.clone();
        let ty = instance.or_else(|| self.expected_instance(template, expected));
        let Some(ty) = ty else {
            self.error(
                Diagnostic::error(
                    codes::CANNOT_INFER,
                    format!("cannot tell which `{type_name}` this is"),
                    span,
                    "type arguments unknown",
                )
                .with_help(format!(
                    "give the type arguments, as in `{type_name}[int].{}(...)`, or add a type hint where the value is used",
                    name.name
                )),
            );
            self.check_all_quietly(cx, args);
            return error_expr(span);
        };
        self.static_call(cx, ty, name, args, span)
    }

    /// The `Reason` shown when an argument does not match a parameter.
    pub(super) fn param_reason(param: &ParamSig) -> Reason {
        Some((param.hint_span, format!("parameter `{}` is declared here", param.name)))
    }
}

/// Read an expression as a type: `int`, `list[str]`, `Pair[str, int]`.
pub(super) fn expr_to_type(expr: &ast::Expr) -> Option<ast::TypeExpr> {
    let kind = match &expr.kind {
        // `User`, or `models.User` from an imported module.
        ast::ExprKind::Name(_) | ast::ExprKind::Attr { .. } => {
            ast::TypeExprKind::Named { name: Checker::dotted(expr)?, args: Vec::new() }
        }
        ast::ExprKind::None => ast::TypeExprKind::None,
        ast::ExprKind::Index { object, index } => {
            let name = Checker::dotted(object)?;
            let items: Vec<&ast::Expr> = match &index.kind {
                ast::ExprKind::Tuple(items) => items.iter().collect(),
                _ => vec![index],
            };
            let args = items.into_iter().map(expr_to_type).collect::<Option<Vec<_>>>()?;
            ast::TypeExprKind::Named { name: name.clone(), args }
        }
        _ => return None,
    };
    Some(ast::TypeExpr { kind, span: expr.span })
}

impl FnCx {
    /// Whether a name is a local here or in an enclosing function.
    pub(super) fn sees(&self, name: &str) -> bool {
        self.scope.contains_key(name) || self.outer.as_ref().is_some_and(|o| o.sees(name))
    }
}
