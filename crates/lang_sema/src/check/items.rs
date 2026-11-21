//! Declaration passes: structs, enums, traits, constants, functions, `impl` blocks and
//! annotations (`@derive`, `@frozen`).

use super::generics::TypeDecl;
use super::{
    BUILTIN_CONSTRUCTORS, BUILTIN_FUNCTIONS, BUILTIN_TYPES, Body, BodyMode, Checker, ConstInfo, EnumInfo, FieldInfo,
    FnCx, Global, ImplInfo, KNOWN_TYPES, ParamSig, Signature, StructInfo, TraitInfo, TraitMethod, VariantInfo, closest,
};
use crate::hir::{self, FuncId};
use crate::types::{EnumId, StructId, TraitId, Ty, TyKind};
use lang_diag::{Diagnostic, Span, codes};
use lang_syntax::ast;
use std::collections::HashMap;

/// The traits `@derive` can implement.
const DERIVABLE: &[&str] = &["Eq", "Hash", "Display", "Copy", "Ord", "Json"];

impl<'m> Checker<'_, 'm> {
    /// Register a new top-level name. Returns false (after reporting) if it is taken.
    fn claim_global(&mut self, name: &ast::Ident) -> bool {
        let builtin = BUILTIN_FUNCTIONS.contains(&name.name.as_str())
            || BUILTIN_CONSTRUCTORS.contains(&name.name.as_str())
            || BUILTIN_TYPES.contains(&name.name.as_str())
            || KNOWN_TYPES.contains(&name.name.as_str());
        let from_prelude = self.globals.get(&name.name).is_some_and(|(_, span)| span.file == self.prelude_file);
        let stdlib = self.stdlib_modules.get(self.current_module).copied().unwrap_or(false) && self.current_module != 0;
        if from_prelude && stdlib && !builtin {
            // A standard library module may reuse a prelude name, as `http.Error` does.
            self.globals.remove(&name.name);
            return true;
        }
        if builtin || from_prelude {
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

    pub(super) fn declare(&mut self, module: &'m ast::Module) {
        for item in &module.items {
            match item {
                ast::Item::Struct(decl) => {
                    if !self.claim_global(&decl.name) {
                        continue;
                    }
                    if !decl.generics.is_empty() {
                        self.declare_generic_type(TypeDecl::Struct(decl));
                        let global = self.globals[&decl.name.name].0;
                        self.export(&decl.name, global, decl.visibility);
                        continue;
                    }
                    let (id, ty) = self.types.add_struct(&decl.name.name);
                    let name = decl.name.name.clone();
                    self.structs.push(StructInfo {
                        name: name.clone(),
                        span: decl.name.span,
                        ty,
                        fields: Vec::new(),
                        // Known now, because sharing checks on field types need it.
                        frozen: decl.annotations.iter().any(|a| a.name.name == "frozen"),
                        generic: false,
                        json_strict: false,
                    });
                    self.globals.insert(name, (Global::Struct(id), decl.name.span));
                    self.export(&decl.name, Global::Struct(id), decl.visibility);
                    self.struct_items.push((id, decl));
                }
                ast::Item::Enum(decl) => {
                    if !self.claim_global(&decl.name) {
                        continue;
                    }
                    if !decl.generics.is_empty() {
                        self.declare_generic_type(TypeDecl::Enum(decl));
                        let global = self.globals[&decl.name.name].0;
                        self.export(&decl.name, global, decl.visibility);
                        continue;
                    }
                    let (id, ty) = self.types.add_enum(&decl.name.name);
                    let name = decl.name.name.clone();
                    self.enums.push(EnumInfo {
                        name: name.clone(),
                        span: decl.name.span,
                        ty,
                        variants: Vec::new(),
                        generic: false,
                    });
                    self.globals.insert(name, (Global::Enum(id), decl.name.span));
                    self.export(&decl.name, Global::Enum(id), decl.visibility);
                    self.enum_items.push((id, decl));
                }
                ast::Item::Trait(decl) => {
                    if !self.claim_global(&decl.name) {
                        continue;
                    }
                    let id = self.types.add_trait(&decl.name.name);
                    let name = decl.name.name.clone();
                    self.traits.push(TraitInfo {
                        name: name.clone(),
                        span: decl.name.span,
                        supertraits: Vec::new(),
                        methods: Vec::new(),
                    });
                    self.globals.insert(name, (Global::Trait(id), decl.name.span));
                    self.export(&decl.name, Global::Trait(id), decl.visibility);
                    self.trait_items.push((id, decl));
                }
                ast::Item::Const(decl) => {
                    if !self.claim_global(&decl.name) {
                        continue;
                    }
                    let index = self.consts.len();
                    self.consts.push(ConstInfo { value: None, state: super::consts::ConstState::Pending });
                    self.globals.insert(decl.name.name.clone(), (Global::Const(index), decl.name.span));
                    self.export(&decl.name, Global::Const(index), decl.visibility);
                    self.const_items.push((index, decl));
                }
                ast::Item::Function(function) => self.declare_function(function),
                ast::Item::Impl(imp) => self.impl_items.push(imp),
                ast::Item::Import(_) => {}
                ast::Item::TypeAlias(alias) => {
                    if !self.claim_global(&alias.name) {
                        continue;
                    }
                    let index = self.aliases.len();
                    self.aliases.push((alias, Err(false)));
                    self.globals.insert(alias.name.name.clone(), (Global::TypeAlias(index), alias.name.span));
                    self.export(&alias.name, Global::TypeAlias(index), alias.visibility);
                }
            }
        }
    }

    /// Declare a top-level function's name, so that other modules can import it before its
    /// signature is resolved by [`Self::collect_functions`].
    fn declare_function(&mut self, function: &'m ast::Function) {
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
            return;
        }
        if !function.generics.is_empty() {
            let index = self.declare_generic_fn(function);
            self.export(&function.name, Global::GenericFunction(index), function.visibility);
            self.fn_items.push((function, super::FnSlot::Generic(index)));
            return;
        }
        let id = FuncId(self.signatures.len());
        self.signatures.push(Signature {
            name: function.name.name.clone(),
            name_span: function.name.span,
            params: Vec::new(),
            ret: Ty::ERROR,
            ret_span: None,
            receiver: None,
            private: false,
        });
        self.bodies.push(Some(Body {
            ast: function,
            self_ty: None,
            bindings: Vec::new(),
            mode: BodyMode::Normal,
            extern_symbol: None,
        }));
        self.globals.insert(function.name.name.clone(), (Global::Function(id), function.name.span));
        self.export(&function.name, Global::Function(id), function.visibility);
        self.fn_items.push((function, super::FnSlot::Plain(id)));
    }

    /// Find the prelude's traits and types that the compiler gives meaning to.
    pub(super) fn find_builtins(&mut self) {
        let trait_named = |this: &Self, name: &str| match this.globals.get(name) {
            Some((Global::Trait(id), _)) => *id,
            other => panic!("the prelude must define the trait {name}, found {other:?}"),
        };
        let builtins = hir::BuiltinTraits {
            display: trait_named(self, "Display"),
            eq: trait_named(self, "Eq"),
            hash: trait_named(self, "Hash"),
            copy: trait_named(self, "Copy"),
            error: trait_named(self, "Error"),
            add: trait_named(self, "AddNumber"),
            sub: trait_named(self, "SubNumber"),
            mul: trait_named(self, "MulNumber"),
            ord: trait_named(self, "Ord"),
            number: trait_named(self, "Number"),
            shareable: trait_named(self, "Shareable"),
            to_json: trait_named(self, "ToJson"),
            from_json: trait_named(self, "FromJson"),
        };
        self.types.error_trait = Some(builtins.error);
        self.builtins = Some(builtins);
        self.parse_error = match self.globals.get("ParseError") {
            Some((Global::Struct(id), _)) => Some(*id),
            other => panic!("the prelude must define ParseError, found {other:?}"),
        };
    }

    /// Resolve the types in trait signatures, struct fields and enum variants.
    pub(super) fn resolve_declarations(&mut self) {
        // Traits first: a field typed as a trait needs the trait's methods to know whether it
        // can be used as a type.
        for (id, decl) in self.trait_items.clone() {
            self.enter_module(self.module_of(decl.name.span));
            let mut supertraits = Vec::new();
            for name in &decl.supertraits {
                match self.globals.get(&name.name) {
                    Some(&(Global::Trait(s), _)) => supertraits.push(s),
                    _ => self.error(Diagnostic::error(
                        codes::UNKNOWN_TYPE,
                        format!("unknown trait `{}`", name.name),
                        name.span,
                        "not a trait",
                    )),
                }
            }
            self.traits[id.0].supertraits = supertraits;
        }
        for (id, decl) in self.trait_items.clone() {
            if self.trait_requires_itself(id) {
                self.error(Diagnostic::error(
                    codes::MISSING_REQUIRED_TRAIT,
                    format!("trait `{}` requires itself", decl.name.name),
                    decl.name.span,
                    "through its list of required traits",
                ));
                self.traits[id.0].supertraits.clear();
            }
        }
        for (id, decl) in self.trait_items.clone() {
            self.enter_module(self.module_of(decl.name.span));
            let self_ty = self.types.self_type(id);
            self.self_ty = Some(self_ty);
            let mut methods: Vec<TraitMethod> = Vec::new();
            for (index, method) in decl.methods.iter().enumerate() {
                if let Some(first) = methods.iter().find(|m| m.name == method.name.name) {
                    let first_span = first.span;
                    self.error(
                        Diagnostic::error(
                            codes::DUPLICATE_DEFINITION,
                            format!("trait `{}` declares `{}` twice", decl.name.name, method.name.name),
                            method.name.span,
                            "declared again here",
                        )
                        .with_label(first_span, "first declared here"),
                    );
                    continue;
                }
                if let Some(param) = method.generics.first() {
                    self.error(
                        Diagnostic::error(
                            codes::NOT_OBJECT_SAFE,
                            format!("trait method `{}` cannot have type parameters", method.name.name),
                            param.name.span,
                            "not allowed in a trait",
                        )
                        .with_help(
                            "a trait can be used as a type, which needs every method to be one function; write a generic function, or give the type parameter to the trait's implementations",
                        ),
                    );
                    continue;
                }
                let signature = self.signature(method, &method.name.name, None);
                methods.push(TraitMethod {
                    name: method.name.name.clone(),
                    span: method.name.span,
                    params: signature.params,
                    ret: signature.ret,
                    ret_span: signature.ret_span,
                    has_self: method.self_param.is_some(),
                });
                if method.has_body {
                    self.trait_defaults.insert((id, methods.len() - 1), &decl.methods[index]);
                }
            }
            self.traits[id.0].methods = methods;
            self.self_ty = None;
        }

        self.resolve_generic_types();
        for (id, decl) in self.struct_items.clone() {
            self.enter_module(self.module_of(decl.name.span));
            let fields = self.fields(&decl.fields);
            self.structs[id.0].fields = fields;
        }
        for (id, decl) in self.enum_items.clone() {
            self.enter_module(self.module_of(decl.name.span));
            let variants = self.variants(&decl.variants, Some(id));
            if variants.is_empty() {
                self.error(Diagnostic::error(
                    codes::BAD_PATTERN,
                    format!("enum `{}` has no variants", decl.name.name),
                    decl.name.span,
                    "needs at least one variant",
                ));
            }
            self.enums[id.0].variants = variants;
        }
    }

    /// The variants of an enum. For a non-generic enum (`owner`), their bare names are recorded
    /// so they can be used without the enum's name.
    pub(super) fn variants(&mut self, decls: &[ast::VariantDecl], owner: Option<EnumId>) -> Vec<VariantInfo> {
        let mut variants: Vec<VariantInfo> = Vec::new();
        for variant in decls {
            if let Some(first) = variants.iter().find(|v| v.name == variant.name.name) {
                let first_span = first.span;
                self.error(
                    Diagnostic::error(
                        codes::DUPLICATE_DEFINITION,
                        format!("variant `{}` is declared twice", variant.name.name),
                        variant.name.span,
                        "declared again here",
                    )
                    .with_label(first_span, "first declared here"),
                );
                continue;
            }
            let fields = self.fields(&variant.fields);
            if let Some(id) = owner {
                self.variant_names.entry(variant.name.name.clone()).or_default().push((id, variants.len()));
            }
            variants.push(VariantInfo { name: variant.name.name.clone(), span: variant.name.span, fields });
        }
        variants
    }

    fn trait_requires_itself(&self, id: TraitId) -> bool {
        let mut stack: Vec<TraitId> = self.traits[id.0].supertraits.clone();
        let mut seen = Vec::new();
        while let Some(t) = stack.pop() {
            if t == id {
                return true;
            }
            if !seen.contains(&t) {
                seen.push(t);
                stack.extend(self.traits[t.0].supertraits.iter().copied());
            }
        }
        false
    }

    pub(super) fn fields(&mut self, decls: &[ast::FieldDecl]) -> Vec<FieldInfo> {
        let mut fields: Vec<FieldInfo> = Vec::new();
        for field in decls {
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
            let json_name = self.field_annotations(field);
            fields.push(FieldInfo {
                name: field.name.name.clone(),
                span: field.ty.span,
                ty,
                default: None,
                private: field.visibility == ast::Visibility::Priv,
                json_name,
            });
        }
        fields
    }

    /// `@json(name="key")` on a field: its key in JSON.
    fn field_annotations(&mut self, field: &ast::FieldDecl) -> Option<String> {
        let mut json_name = None;
        for annotation in &field.annotations {
            if annotation.name.name != "json" {
                self.error(
                    Diagnostic::error(
                        codes::BAD_ANNOTATION,
                        format!("unknown annotation `@{}` on a field", annotation.name.name),
                        annotation.name.span,
                        "not a known annotation",
                    )
                    .with_help("fields can have `@json(name=\"key\")`"),
                );
                continue;
            }
            for arg in &annotation.args {
                match (&arg.keyword, &arg.value) {
                    (Some(keyword), ast::AnnotationValue::Str(name)) if keyword.name == "name" => {
                        json_name = Some(name.clone());
                    }
                    _ => self.error(Diagnostic::error(
                        codes::BAD_ANNOTATION,
                        "`@json` on a field takes `name=\"key\"`",
                        arg.span,
                        "not a known option",
                    )),
                }
            }
        }
        json_name
    }

    // ---- annotations --------------------------------------------------------------------

    pub(super) fn register_derives(&mut self) {
        let items: Vec<(Ty, &'m [ast::Annotation], bool)> = self
            .struct_items
            .iter()
            .map(|(id, d)| (self.structs[id.0].ty, d.annotations.as_slice(), true))
            .chain(self.enum_items.iter().map(|(id, d)| (self.enums[id.0].ty, d.annotations.as_slice(), false)))
            .collect();
        for (ty, annotations, is_struct) in items {
            for annotation in annotations {
                match annotation.name.name.as_str() {
                    "derive" => {
                        if annotation.args.is_empty() {
                            self.error(Diagnostic::error(
                                codes::BAD_ANNOTATION,
                                "`@derive` needs a list of traits",
                                annotation.span,
                                "for example `@derive(Eq, Display)`",
                            ));
                        }
                        for arg in self.annotation_names(annotation) {
                            self.derive(ty, arg);
                        }
                    }
                    "frozen" if is_struct => {
                        if let Some(id) = self.struct_of(ty) {
                            self.structs[id.0].frozen = true;
                        }
                    }
                    "json" if is_struct => {
                        for arg in self.annotation_names(annotation) {
                            if arg.name == "strict" {
                                if let Some(id) = self.struct_of(ty) {
                                    self.structs[id.0].json_strict = true;
                                }
                            } else {
                                self.error(Diagnostic::error(
                                    codes::BAD_ANNOTATION,
                                    format!("unknown option `{}` for `@json` on a struct", arg.name),
                                    arg.span,
                                    "the struct option is `strict`",
                                ));
                            }
                        }
                    }
                    "frozen" => self.error(Diagnostic::error(
                        codes::BAD_ANNOTATION,
                        "`@frozen` only applies to structs",
                        annotation.span,
                        "enums are already immutable",
                    )),
                    other => {
                        let mut diagnostic = Diagnostic::error(
                            codes::BAD_ANNOTATION,
                            format!("unknown annotation `@{other}`"),
                            annotation.name.span,
                            "not a known annotation",
                        )
                        .with_help(
                            "the annotations of structs and enums are `@derive(...)`, `@frozen` and `@json(strict)`",
                        );
                        if matches!(other, "test") {
                            diagnostic = Diagnostic::error(
                                codes::NOT_SUPPORTED,
                                format!("`@{other}` is not supported yet"),
                                annotation.name.span,
                                "not supported yet",
                            );
                        }
                        self.error(diagnostic);
                    }
                }
            }
        }
    }

    /// The arguments of an annotation that takes names, such as `@derive(Eq, Hash)`, reporting
    /// any that are not names.
    pub(super) fn annotation_names<'a>(&mut self, annotation: &'a ast::Annotation) -> Vec<&'a ast::Ident> {
        let mut names = Vec::new();
        for arg in &annotation.args {
            match (&arg.keyword, &arg.value) {
                (None, ast::AnnotationValue::Name(name)) => names.push(name),
                _ => self.error(Diagnostic::error(
                    codes::BAD_ANNOTATION,
                    format!("`@{}` takes trait names", annotation.name.name),
                    arg.span,
                    "not a name",
                )),
            }
        }
        names
    }

    fn derive(&mut self, ty: Ty, arg: &ast::Ident) {
        for trait_id in self.derivable(arg) {
            let methods = vec![None; self.traits[trait_id.0].methods.len()];
            self.add_impl(ImplInfo { trait_id, ty, span: arg.span, methods, derived: true });
        }
    }

    /// The traits `@derive(name)` implements (`Json` is `ToJson` and `FromJson`), or none after
    /// reporting that it cannot be derived.
    pub(super) fn derivable(&mut self, arg: &ast::Ident) -> Vec<TraitId> {
        let b = self.builtin();
        vec![match arg.name.as_str() {
            "Eq" => b.eq,
            "Hash" => b.hash,
            "Display" => b.display,
            "Copy" => b.copy,
            "Ord" => b.ord,
            "Json" => return vec![b.to_json, b.from_json],
            other => {
                let mut diagnostic = Diagnostic::error(
                    codes::BAD_ANNOTATION,
                    format!("`{other}` cannot be derived"),
                    arg.span,
                    "not derivable",
                )
                .with_help(format!("the derivable traits are {}", DERIVABLE.join(", ")));
                if let Some(s) = closest(other, DERIVABLE.iter().copied()) {
                    diagnostic = diagnostic.with_help(format!("did you mean `{s}`?"));
                }
                self.error(diagnostic);
                return Vec::new();
            }
        }]
    }

    pub(super) fn add_impl(&mut self, info: ImplInfo) -> bool {
        if let Some(&existing) = self.impl_index.get(&(info.trait_id, info.ty)) {
            let first = self.impls[existing].span;
            let (ty, tr) = (self.display(info.ty), self.traits[info.trait_id.0].name.clone());
            self.error(
                Diagnostic::error(
                    codes::DUPLICATE_DEFINITION,
                    format!("`{ty}` already implements `{tr}`"),
                    info.span,
                    "implemented again here",
                )
                .with_label(first, "first implemented here"),
            );
            return false;
        }
        for (index, method) in self.traits[info.trait_id.0].methods.iter().enumerate() {
            self.trait_methods.entry((info.ty, method.name.clone())).or_default().push((info.trait_id, index));
        }
        self.impl_index.insert((info.trait_id, info.ty), self.impls.len());
        self.impls.push(info);
        true
    }

    // ---- functions and impls ------------------------------------------------------------

    /// Resolve the signatures of top-level functions, and collect `impl` blocks.
    pub(super) fn collect_functions(&mut self) {
        for (function, slot) in self.fn_items.clone() {
            self.enter_module(self.module_of(function.name.span));
            match slot {
                super::FnSlot::Generic(index) => self.finish_generic_fn(index),
                super::FnSlot::Plain(id) => {
                    let name = self.qualified(&function.name.name);
                    let mut signature = self.signature(function, &name, None);
                    signature.private = false;
                    let symbol = self.function_annotations(function, &signature);
                    self.signatures[id.0] = signature;
                    if let Some(Some(body)) = self.bodies.get_mut(id.0) {
                        body.extern_symbol = symbol;
                    }
                }
            }
        }
        for imp in self.impl_items.clone() {
            self.enter_module(self.module_of(imp.target.span));
            self.collect_impl(imp);
        }
    }

    /// Check a function's annotations; returns the C symbol for `@extern`.
    fn function_annotations(&mut self, function: &ast::Function, signature: &Signature) -> Option<String> {
        for annotation in &function.annotations {
            // Routes are checked once every signature is known.
            if annotation.name.name == "test" {
                self.check_test_signature(function, signature);
                continue;
            }
            if self.auth_annotation(annotation) {
                continue;
            }
            if annotation.name.name != "extern" && !annotation.name.name.starts_with("http.") {
                self.error(
                    Diagnostic::error(
                        codes::BAD_ANNOTATION,
                        format!("unknown annotation `@{}` on a function", annotation.name.name),
                        annotation.name.span,
                        "not a known annotation",
                    )
                    .with_help("functions can have `@test`, `@extern(\"c_name\")`, the `@http` route annotations and `@auth.required`"),
                );
            }
        }
        self.extern_symbol(function, signature)
    }

    /// A `@test` function takes nothing and returns nothing or `Result[None]`; `test` runs it.
    fn check_test_signature(&mut self, function: &ast::Function, signature: &Signature) {
        let returns_ok =
            signature.ret == Ty::NONE || self.types.result_parts(signature.ret).is_some_and(|(ok, _)| ok == Ty::NONE);
        if !signature.params.is_empty() || function.self_param.is_some() || !function.generics.is_empty() || !returns_ok
        {
            self.error(
                Diagnostic::error(
                    codes::BAD_ANNOTATION,
                    format!(
                        "test `{}` must take no parameters and return nothing or `Result[None]`",
                        function.name.name
                    ),
                    function.name.span,
                    "not a test function's signature",
                )
                .with_help("a test is `@test def name():`; it fails if it panics (as `assert` does) or returns `Err`"),
            );
        }
    }

    /// What a name in `impl Name for ...` refers to: a trait in scope, or else a prelude trait
    /// whose name the module reuses for something else (`impl Error for HttpError` in `http`).
    pub(super) fn trait_named(&self, name: &str) -> Option<Global> {
        match self.globals.get(name) {
            Some(&(global @ Global::Trait(_), _)) => Some(global),
            other => match self.exports[0].get(name) {
                Some(&(global @ Global::Trait(_), _, _)) => Some(global),
                _ => other.map(|(g, _)| *g),
            },
        }
    }

    fn impl_target(&mut self, target: &ast::Ident) -> Option<Ty> {
        match self.globals.get(&target.name) {
            Some(&(Global::Struct(id), _)) => Some(self.structs[id.0].ty),
            Some(&(Global::Enum(id), _)) => Some(self.enums[id.0].ty),
            Some(&(Global::GenericStruct(_) | Global::GenericEnum(_), _)) => {
                self.error(
                    Diagnostic::error(
                        codes::UNEXPECTED_TYPE_ARGS,
                        format!("`{}` is generic; say which type parameters the `impl` block has", target.name),
                        target.span,
                        "type arguments missing",
                    )
                    .with_help(format!("write `impl[T] {}[T]:`", target.name)),
                );
                None
            }
            _ => {
                let message = if KNOWN_TYPES.contains(&target.name.as_str()) {
                    format!("cannot add methods or traits to the built-in type `{}`", target.name)
                } else if let Some(kind) = self.global_kind(&target.name) {
                    format!("`{}` is {kind}, not a struct or enum", target.name)
                } else {
                    format!("`impl {}` needs a struct or enum named `{}` in this file", target.name, target.name)
                };
                self.error(Diagnostic::error(codes::UNKNOWN_TYPE, message, target.span, "not a struct or enum"));
                None
            }
        }
    }

    fn collect_impl(&mut self, imp: &'m ast::ImplDecl) {
        if let Some(&(Global::GenericStruct(index) | Global::GenericEnum(index), _)) =
            self.globals.get(&imp.target.name)
            && !imp.target_args.is_empty()
        {
            self.declare_impl_template(imp, index);
            return;
        }
        if let Some(param) = imp.generics.first() {
            self.error(Diagnostic::error(
                codes::UNEXPECTED_TYPE_ARGS,
                format!("`{}` is not generic, so the `impl` block cannot have type parameters", imp.target.name),
                param.name.span,
                "remove the `[...]` after `impl`",
            ));
            return;
        }
        if let Some(arg) = imp.target_args.first() {
            self.error(Diagnostic::error(
                codes::UNEXPECTED_TYPE_ARGS,
                format!("the type `{}` does not take type arguments", imp.target.name),
                arg.span,
                "remove the `[...]`",
            ));
            return;
        }
        let Some(target) = self.impl_target(&imp.target) else { return };
        let type_name = imp.target.name.clone();
        match &imp.trait_name {
            None => self.collect_inherent(imp, target, &type_name, &[], BodyMode::Normal),
            Some(trait_name) => match self.trait_named(&trait_name.name) {
                Some(Global::Trait(trait_id)) => {
                    self.collect_trait_impl(imp, trait_id, target, &type_name, &[], BodyMode::Normal)
                }
                _ => {
                    let message = match self.global_kind(&trait_name.name) {
                        Some(kind) => format!("`{}` is {kind}, not a trait", trait_name.name),
                        None => format!("unknown trait `{}`", trait_name.name),
                    };
                    self.error(Diagnostic::error(codes::UNKNOWN_TYPE, message, trait_name.span, "not a trait"));
                }
            },
        }
    }

    pub(super) fn collect_inherent(
        &mut self,
        imp: &'m ast::ImplDecl,
        target: Ty,
        type_name: &str,
        bindings: &[(String, Ty)],
        mode: BodyMode,
    ) {
        for method in &imp.methods {
            let name = &method.name.name;
            if !method.generics.is_empty() {
                if self.inherent.contains_key(&(target, name.clone()))
                    || self.generic_methods.contains_key(&(target, name.clone()))
                {
                    self.error(Diagnostic::error(
                        codes::DUPLICATE_DEFINITION,
                        format!("`{type_name}` already has a method named `{name}`"),
                        method.name.span,
                        "defined again here",
                    ));
                    continue;
                }
                self.declare_generic_method(method, target, type_name, bindings, mode);
                continue;
            }
            if self.generic_methods.contains_key(&(target, name.clone())) {
                self.error(Diagnostic::error(
                    codes::DUPLICATE_DEFINITION,
                    format!("`{type_name}` already has a method named `{name}`"),
                    method.name.span,
                    "defined again here",
                ));
                continue;
            }
            if let Some(&existing) = self.inherent.get(&(target, name.clone())) {
                let first = self.signatures[existing.0].name_span;
                self.error(
                    Diagnostic::error(
                        codes::DUPLICATE_DEFINITION,
                        format!("`{type_name}` already has a method named `{name}`"),
                        method.name.span,
                        "defined again here",
                    )
                    .with_label(first, "first defined here"),
                );
                continue;
            }
            if let Some(id) = self.struct_of(target)
                && let Some(field) = self.structs[id.0].fields.iter().find(|f| &f.name == name)
            {
                let field_span = field.span;
                self.error(
                    Diagnostic::error(
                        codes::DUPLICATE_DEFINITION,
                        format!("`{type_name}` already has a field named `{name}`"),
                        method.name.span,
                        "method with the same name as a field",
                    )
                    .with_label(field_span, "field declared here"),
                );
                continue;
            }
            self.self_ty = Some(target);
            let receiver = method.self_param.map(|span| (target, span));
            let mut signature = self.signature(method, &format!("{type_name}.{name}"), receiver);
            signature.private = method.visibility == ast::Visibility::Priv;
            self.self_ty = None;
            let mut other = None;
            for annotation in &method.annotations {
                if !annotation.name.name.starts_with("http.") && !self.auth_annotation(annotation) && other.is_none() {
                    other = Some(annotation);
                }
            }
            if let Some(annotation) = other {
                self.error(Diagnostic::error(
                    codes::BAD_ANNOTATION,
                    format!("annotation `@{}` is not allowed on a method", annotation.name.name),
                    annotation.name.span,
                    "not allowed here",
                ));
            }
            let id = FuncId(self.signatures.len());
            self.signatures.push(signature);
            self.bodies.push(Some(Body {
                ast: method,
                self_ty: Some(target),
                bindings: bindings.to_vec(),
                mode,
                extern_symbol: None,
            }));
            self.inherent.insert((target, name.clone()), id);
        }
    }

    pub(super) fn collect_trait_impl(
        &mut self,
        imp: &'m ast::ImplDecl,
        trait_id: TraitId,
        target: Ty,
        type_name: &str,
        bindings: &[(String, Ty)],
        mode: BodyMode,
    ) {
        let trait_name = self.traits[trait_id.0].name.clone();
        let span = imp.trait_name.as_ref().map_or(imp.target.span, |t| t.span.to(imp.target.span));
        if trait_id == self.builtin().shareable {
            self.error(
                Diagnostic::error(codes::BAD_IMPL, "`Shareable` cannot be implemented by hand", span, "decided by the compiler")
                    .with_help("a type is shareable when it cannot be changed: mark a struct `@frozen` and give it shareable fields"),
            );
            return;
        }
        let provided: HashMap<&str, &'m ast::Function> =
            imp.methods.iter().map(|m| (m.name.name.as_str(), m)).collect();
        for method in &imp.methods {
            if !self.traits[trait_id.0].methods.iter().any(|m| m.name == method.name.name) {
                let candidates: Vec<String> = self.traits[trait_id.0].methods.iter().map(|m| m.name.clone()).collect();
                let mut diagnostic = Diagnostic::error(
                    codes::BAD_IMPL,
                    format!("`{}` is not a method of the trait `{trait_name}`", method.name.name),
                    method.name.span,
                    "not in the trait",
                )
                .with_help(format!(
                    "methods that are not part of `{trait_name}` go in a separate `impl {type_name}:` block"
                ));
                if let Some(s) = closest(&method.name.name, candidates.iter().map(String::as_str)) {
                    diagnostic = diagnostic.with_help(format!("did you mean `{s}`?"));
                }
                self.error(diagnostic);
            }
        }

        let mut methods = Vec::new();
        let mut missing = Vec::new();
        let count = self.traits[trait_id.0].methods.len();
        for index in 0..count {
            let method_name = self.traits[trait_id.0].methods[index].name.clone();
            let func = FuncId(self.signatures.len());
            if let Some(&ast_method) = provided.get(method_name.as_str()) {
                self.self_ty = Some(target);
                let receiver = ast_method.self_param.map(|s| (target, s));
                let signature = self.signature(ast_method, &format!("{type_name}.{method_name}"), receiver);
                self.self_ty = None;
                self.check_impl_signature(trait_id, index, target, &signature, ast_method);
                self.signatures.push(signature);
                self.bodies.push(Some(Body {
                    ast: ast_method,
                    self_ty: Some(target),
                    bindings: bindings.to_vec(),
                    mode,
                    extern_symbol: None,
                }));
                methods.push(Some(func));
            } else if let Some(&default) = self.trait_defaults.get(&(trait_id, index)) {
                // A copy of the trait's default method, with `Self` as this type.
                let method = &self.traits[trait_id.0].methods[index];
                let (params, ret, ret_span, has_self) =
                    (method.params.clone(), method.ret, method.ret_span, method.has_self);
                let params = params.into_iter().map(|p| ParamSig { ty: self.subst_self(p.ty, target), ..p }).collect();
                let ret = self.subst_self(ret, target);
                self.signatures.push(Signature {
                    name: format!("{type_name}.{method_name}"),
                    name_span: default.name.span,
                    params,
                    ret,
                    ret_span,
                    receiver: has_self.then_some((target, imp.target.span)),
                    private: false,
                });
                let mode = if mode == BodyMode::Skip { BodyMode::Skip } else { BodyMode::Quiet };
                self.bodies.push(Some(Body {
                    ast: default,
                    self_ty: Some(target),
                    bindings: bindings.to_vec(),
                    mode,
                    extern_symbol: None,
                }));
                methods.push(Some(func));
            } else {
                missing.push(method_name);
                methods.push(None);
            }
        }
        if !missing.is_empty() {
            let list = missing.iter().map(|m| format!("`{m}`")).collect::<Vec<_>>().join(", ");
            let word = if missing.len() == 1 { "method" } else { "methods" };
            let trait_span = self.traits[trait_id.0].span;
            self.error(
                Diagnostic::error(
                    codes::BAD_IMPL,
                    format!("`impl {trait_name} for {type_name}` is missing the {word} {list}"),
                    span,
                    format!("missing {list}"),
                )
                .with_label(trait_span, format!("`{trait_name}` is declared here")),
            );
            // Registered anyway, so calls to the methods it does have are not reported too.
        }
        self.add_impl(ImplInfo { trait_id, ty: target, span, methods, derived: false });
    }

    /// An implementing method must have the trait's signature, with `Self` as the type.
    fn check_impl_signature(
        &mut self,
        trait_id: TraitId,
        index: usize,
        target: Ty,
        signature: &Signature,
        method: &ast::Function,
    ) {
        let expected = &self.traits[trait_id.0].methods[index];
        let (has_self, expected_span, params, ret) =
            (expected.has_self, expected.span, expected.params.clone(), expected.ret);
        let trait_name = self.traits[trait_id.0].name.clone();
        let problem = if has_self != signature.receiver.is_some() {
            Some(if has_self {
                "the trait's method takes `self`".to_string()
            } else {
                "the trait's method does not take `self`".to_string()
            })
        } else if params.len() != signature.params.len() {
            Some(format!("the trait's method takes {}", super::plural(params.len(), "parameter")))
        } else {
            let mut found = None;
            for (want, got) in params.iter().zip(&signature.params) {
                let want_ty = self.subst_self(want.ty, target);
                if !self.types.fits(got.ty, want_ty) {
                    found = Some(format!("parameter `{}` should be `{}`", got.name, self.display(want_ty)));
                    break;
                }
            }
            let want_ret = self.subst_self(ret, target);
            if found.is_none() && !self.types.fits(signature.ret, want_ret) {
                found = Some(format!("the return type should be `{}`", self.display(want_ret)));
            }
            found
        };
        if let Some(problem) = problem {
            self.error(
                Diagnostic::error(
                    codes::BAD_IMPL,
                    format!("`{}` does not match its declaration in `{trait_name}`", method.name.name),
                    method.name.span,
                    problem,
                )
                .with_label(expected_span, "declared here"),
            );
        }
    }

    pub(super) fn signature(
        &mut self,
        function: &ast::Function,
        name: &str,
        receiver: Option<(Ty, Span)>,
    ) -> Signature {
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
            let default = param.default.as_ref().map(|value| self.default_value(value, ty, hint_span));
            params.push(ParamSig { name: param.name.name.clone(), ty, hint_span, default });
        }
        let ret = function.ret.as_ref().map_or(Ty::NONE, |hint| self.resolve_type(hint));
        Signature {
            name: name.to_string(),
            name_span: function.name.span,
            params,
            ret,
            ret_span: function.ret.as_ref().map(|h| h.span),
            receiver,
            private: false,
        }
    }

    /// A parameter's default value, evaluated afresh at each call that leaves it out. It cannot
    /// use other parameters or local variables.
    fn default_value(&mut self, value: &ast::Expr, ty: Ty, hint_span: Span) -> hir::Expr {
        let mut cx = FnCx::detached();
        let reason = Some((hint_span, "expected because of the parameter's type".to_string()));
        let checked = self.expect(&mut cx, value, ty, reason);
        if !cx.locals.is_empty() {
            self.error(
                Diagnostic::error(
                    codes::NOT_SUPPORTED,
                    "this default value is too complex",
                    value.span,
                    "not supported as a default",
                )
                .with_help("use a literal, a constant, a list or dict, or a constructor call"),
            );
            return super::error_expr(value.span);
        }
        checked
    }

    // ---- requirements -------------------------------------------------------------------

    /// An implementation of a trait needs implementations of the traits it requires.
    pub(super) fn check_impl_requirements(&mut self) {
        for index in 0..self.impls.len() {
            let (trait_id, ty, span, derived) =
                (self.impls[index].trait_id, self.impls[index].ty, self.impls[index].span, self.impls[index].derived);
            for required in self.traits[trait_id.0].supertraits.clone() {
                if self.implements(ty, required) {
                    continue;
                }
                let (ty_name, tr, req) =
                    (self.display(ty), self.traits[trait_id.0].name.clone(), self.traits[required.0].name.clone());
                let help = if derived {
                    format!("add `{req}` to the `@derive(...)` list")
                } else {
                    format!("implement `{req}` for `{ty_name}` as well")
                };
                self.error(
                    Diagnostic::error(
                        codes::MISSING_REQUIRED_TRAIT,
                        format!("`{ty_name}` implements `{tr}` but not `{req}`, which `{tr}` requires"),
                        span,
                        format!("`{tr}` requires `{req}`"),
                    )
                    .with_help(help),
                );
            }
        }
    }

    /// A derived implementation needs every field to support the same operation.
    pub(super) fn check_derives(&mut self) {
        let b = self.builtin();
        for index in 0..self.impls.len() {
            if !self.impls[index].derived {
                continue;
            }
            let (trait_id, ty, span) = (self.impls[index].trait_id, self.impls[index].ty, self.impls[index].span);
            // Instances of generic types get derived traits only when their type arguments have
            // them; the pattern instance is checked as if every parameter had the trait.
            let mut assumed: Vec<u32> = Vec::new();
            if let Some((template, args)) = self.instance_of.get(&ty).cloned() {
                if !self.is_pattern_instance(ty) {
                    continue;
                }
                for param in &self.generic_types[template].params {
                    assumed.push(param.index);
                }
                let _ = args;
                for p in &assumed {
                    self.params[*p as usize].push(trait_id);
                }
            }
            let fields: Vec<FieldInfo> = match self.types.kind(ty) {
                TyKind::Struct(id) => self.structs[id.0].fields.clone(),
                TyKind::Enum(id) => self.enums[id.0].variants.iter().flat_map(|v| v.fields.clone()).collect(),
                _ => Vec::new(),
            };
            for field in fields {
                let (ok, needs) = if trait_id == b.eq {
                    (self.equatable(field.ty), "be compared with `==`")
                } else if trait_id == b.hash {
                    (self.hashable(field.ty), "be hashed")
                } else if trait_id == b.display {
                    (self.displayable(field.ty), "be printed")
                } else if trait_id == b.ord {
                    (self.orderable(field.ty), "be ordered with `<`")
                } else if trait_id == b.to_json {
                    (self.json_capable(field.ty), "be written as JSON")
                } else {
                    (true, "")
                };
                if !ok {
                    let (tr, shown) = (self.traits[trait_id.0].name.clone(), self.display(field.ty));
                    self.error(
                        Diagnostic::error(
                            codes::BAD_ANNOTATION,
                            format!(
                                "cannot derive `{tr}`: field `{}` has type `{shown}`, which cannot {needs}",
                                field.name
                            ),
                            span,
                            "cannot be derived",
                        )
                        .with_label(field.span, format!("`{}` is declared here", field.name)),
                    );
                    break;
                }
            }
            for p in &assumed {
                self.params[*p as usize].pop();
            }
        }
    }

    // ---- constants, defaults, main ------------------------------------------------------

    pub(super) fn check_field_defaults(&mut self) {
        for (id, decl) in self.struct_items.clone() {
            self.enter_module(self.module_of(decl.name.span));
            self.field_defaults(id, decl);
        }
    }

    pub(super) fn field_defaults(&mut self, id: StructId, decl: &ast::StructDecl) {
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

    pub(super) fn check_main(&mut self, module: &ast::Module, entry: &str) -> Option<FuncId> {
        self.enter_module(self.module_names.len() - 1);
        let Some(&(Global::Function(id), _)) = self.globals.get(entry) else {
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

    /// Check each trait default method once, with `self` standing for any implementing type.
    /// Each implementation gets its own copy, checked quietly, when the bodies are checked.
    pub(super) fn check_trait_defaults(&mut self) {
        let mut defaults: Vec<((TraitId, usize), &'m ast::Function)> =
            self.trait_defaults.iter().map(|(k, v)| (*k, *v)).collect();
        defaults.sort_by_key(|((t, i), _)| (t.0, *i));
        for ((trait_id, index), ast_method) in defaults {
            self.enter_module(self.module_of(ast_method.name.span));
            let self_ty = self.types.self_type(trait_id);
            let method = &self.traits[trait_id.0].methods[index];
            let signature = Signature {
                name: format!("{}.{}", self.traits[trait_id.0].name, method.name),
                name_span: method.span,
                params: method.params.clone(),
                ret: method.ret,
                ret_span: method.ret_span,
                receiver: method.has_self.then_some((self_ty, method.span)),
                private: false,
            };
            self.check_body(&signature, ast_method, Some(self_ty));
        }
    }
}
