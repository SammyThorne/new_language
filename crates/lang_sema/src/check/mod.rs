//! The type checker (docs/02-type-system-and-traits.md): every parameter and field has a type
//! hint, locals take their type from their first assignment, traits are checked for complete and
//! consistent implementations, `match` must be exhaustive, and any mismatch is an error.
//!
//! The prelude (`prelude.lang`) is checked together with the program, so the built-in traits
//! (`Display`, `Eq`, `Hash`, `Copy`, `Error`, the number traits) are ordinary traits.
//!
//! Passes, in order (`items.rs` has the declaration passes):
//! 1. Declare every struct, enum, trait and constant, so types can refer to later declarations.
//! 2. Resolve field, variant and trait signature types.
//! 3. Register derived implementations, then collect functions, methods and `impl` blocks,
//!    copying trait default methods into each implementation that does not override them.
//! 4. Check that implementations meet their requirements, and that derives are possible.
//! 5. Check constants, field defaults, and `main`.
//! 6. Check every body, producing HIR; then check trait default methods once on their own.
//!
//! The other files: statements (`stmt.rs`), expressions (`expr.rs`), built-in functions and
//! methods (`builtins.rs`), and `match` patterns with exhaustiveness (`patterns.rs`).

mod builtins;
mod closures;
mod expr;
mod generics;
mod items;
mod modules;
mod patterns;
mod stmt;

use crate::hir::{self, FuncId, LocalId};
use crate::types::{EnumId, StructId, TraitId, Ty, TyKind, Types};
use lang_diag::{Diagnostic, Diagnostics, FileId, SourceMap, Span, codes};
use lang_syntax::ast;
use std::collections::HashMap;

/// The prelude's source, compiled with every program.
pub const PRELUDE: &str = include_str!("../prelude.lang");

/// The file name the prelude has in diagnostics.
pub const PRELUDE_NAME: &str = "<prelude>";

/// Functions provided by the compiler rather than by source code.
const BUILTIN_FUNCTIONS: &[&str] = &[
    "print",
    "len",
    "str",
    "int",
    "float",
    "abs",
    "panic",
    "range",
    "min",
    "max",
    "sum",
    "sorted",
    "enumerate",
    "zip",
    "any",
    "all",
    "input",
    "i8",
    "i16",
    "i32",
    "u8",
    "u16",
    "u32",
    "u64",
    "f32",
];

/// Constructors of the built-in enums.
const BUILTIN_CONSTRUCTORS: &[&str] = &["Some", "Ok", "Err"];

/// Built-in type names, which cannot be redefined.
const BUILTIN_TYPES: &[&str] = &["bool", "list", "dict", "tuple", "set", "bytes", "None", "Result", "Self"];

/// Type names that exist in the language design but are not implemented yet.
const PLANNED_TYPES: &[&str] = &["set", "bytes", "bytearray", "frozenset", "range"];

const KNOWN_TYPES: &[&str] = &[
    "int", "bool", "str", "float", "list", "dict", "tuple", "None", "Result", "i8", "i16", "i32", "i64", "u8", "u16",
    "u32", "u64", "f32", "f64",
];

/// One module of a program: its dotted name (`services.billing`) and its syntax tree.
pub struct ModuleInput<'a> {
    pub name: String,
    pub ast: &'a ast::Module,
}

/// Check a single-file program together with the prelude, which is added to `sources`. Returns
/// HIR when this phase found no errors.
pub fn check(
    module: &ast::Module,
    sources: &mut SourceMap,
    module_name: &str,
    diags: &mut Diagnostics,
) -> Option<hir::Program> {
    check_modules(&[ModuleInput { name: module_name.to_string(), ast: module }], sources, diags)
}

/// Check a program made of several modules. The last module is the entry point, with `main`;
/// the loader has already found every module the others import, and rejected import cycles.
pub fn check_modules(inputs: &[ModuleInput], sources: &mut SourceMap, diags: &mut Diagnostics) -> Option<hir::Program> {
    let prelude_file = sources.add(PRELUDE_NAME, PRELUDE);
    let mut prelude_diags = Diagnostics::default();
    let tokens = lang_syntax::lex(prelude_file, PRELUDE, &mut prelude_diags);
    let prelude = lang_syntax::parse(&tokens, &mut prelude_diags);
    assert!(!prelude_diags.has_errors(), "the prelude does not parse");

    let errors_before = diags.error_count();
    let mut checker = Checker::new(diags, prelude_file);
    let modules: Vec<(String, &ast::Module)> = std::iter::once((PRELUDE_NAME.to_string(), &prelude))
        .chain(inputs.iter().map(|m| (m.name.clone(), m.ast)))
        .collect();
    let module = modules.last().expect("at least one module").1;
    let module_name = modules.last().expect("at least one module").0.clone();
    checker.start_modules(&modules);
    for (index, (_, m)) in modules.iter().enumerate() {
        checker.enter_module(index);
        checker.declare(m);
        if index == 0 {
            checker.find_builtins();
        }
    }
    checker.import_all(&modules);
    checker.resolve_declarations();
    checker.check_consts();
    checker.register_derives();
    checker.collect_functions();
    checker.finish_generic_setup();
    checker.check_impl_requirements();
    checker.check_derives();
    checker.check_pending_hashable();
    checker.check_field_defaults();
    let entry = checker.check_main(module);
    // Checking bodies can instantiate generic functions and types, adding more bodies.
    let mut functions: Vec<Option<hir::Function>> = Vec::new();
    let mut index = 0;
    while index < checker.bodies.len() {
        let function = checker.function(FuncId(index));
        functions.resize_with(checker.bodies.len().max(index + 1), || None);
        functions[index] = function;
        index += 1;
    }
    functions.resize_with(checker.bodies.len(), || None);
    for (id, function) in std::mem::take(&mut checker.closures) {
        functions[id.0] = Some(function);
    }
    let functions: Vec<hir::Function> = functions.into_iter().map(|f| f.expect("every function was checked")).collect();
    checker.check_trait_defaults();
    checker.check_pending_hashable();
    if checker.diags.error_count() == errors_before
        && let Some(diagnostic) = checker.diags.suppressed.take()
    {
        // An error that only shows up in an instance of generic code (or a copy of a trait's
        // default method) was not reported when the original was checked; report it now.
        checker.diags.out.push(diagnostic.with_help(
            "this error is in generic code or a trait's default method, for the types used at one of its uses",
        ));
    }
    if checker.diags.error_count() > errors_before {
        return None;
    }
    Some(checker.into_program(&module_name, functions, entry?))
}

#[derive(Clone, Copy, Debug)]
enum Global {
    Function(FuncId),
    Struct(StructId),
    Enum(EnumId),
    Trait(TraitId),
    Const(usize),
    /// Indexes into the generic templates.
    GenericFunction(usize),
    GenericStruct(usize),
    GenericEnum(usize),
    /// An imported module, by index.
    Module(usize),
    /// A name whose lookup already failed and was reported.
    Missing,
}

struct StructInfo {
    name: String,
    span: Span,
    ty: Ty,
    fields: Vec<FieldInfo>,
    frozen: bool,
    /// An instance of a generic struct with type parameters still in its arguments.
    generic: bool,
}

#[derive(Clone)]
struct FieldInfo {
    name: String,
    /// The field's type hint.
    span: Span,
    ty: Ty,
    default: Option<hir::Expr>,
    /// Declared `priv`: only visible in the file that defines the type.
    private: bool,
}

struct EnumInfo {
    name: String,
    span: Span,
    ty: Ty,
    variants: Vec<VariantInfo>,
    generic: bool,
}

struct VariantInfo {
    name: String,
    span: Span,
    fields: Vec<FieldInfo>,
}

struct TraitInfo {
    name: String,
    span: Span,
    /// Direct required traits.
    supertraits: Vec<TraitId>,
    methods: Vec<TraitMethod>,
}

struct TraitMethod {
    name: String,
    span: Span,
    /// Parameter types after `self`; may mention `Self`.
    params: Vec<ParamSig>,
    ret: Ty,
    ret_span: Option<Span>,
    has_self: bool,
}

struct ImplInfo {
    trait_id: TraitId,
    ty: Ty,
    span: Span,
    methods: Vec<Option<FuncId>>,
    derived: bool,
}

#[derive(Clone)]
struct Signature {
    /// `name`, or `Type.name` for methods.
    name: String,
    name_span: Span,
    params: Vec<ParamSig>,
    ret: Ty,
    ret_span: Option<Span>,
    /// For methods taking `self`: the type of `self`, and where it is written.
    receiver: Option<(Ty, Span)>,
    /// A method declared `priv`: only callable in the file that defines it.
    private: bool,
}

#[derive(Clone)]
struct ParamSig {
    name: String,
    ty: Ty,
    /// The type hint, or the parameter name when the hint is missing.
    hint_span: Span,
    /// Used when a call leaves the argument out.
    default: Option<hir::Expr>,
}

struct ConstInfo {
    /// `None` when the constant's own definition had an error.
    value: Option<hir::Expr>,
}

/// A top-level function declared in the first pass, whose signature is resolved later.
#[derive(Clone, Copy)]
enum FnSlot {
    Plain(FuncId),
    Generic(usize),
}

/// How a body is checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BodyMode {
    Normal,
    /// Copies of trait default methods and instances of generic code: their errors were
    /// reported when the original was checked on its own, so they are not reported again.
    Quiet,
    /// A generic declaration checked against its bounds; reported, but not compiled.
    GenericCheck,
    /// An instance whose types are still parameters: only its signature is needed.
    Skip,
}

/// A function body to check, with the meaning of `Self` and of type parameters inside it.
struct Body<'m> {
    ast: &'m ast::Function,
    self_ty: Option<Ty>,
    bindings: Vec<(String, Ty)>,
    mode: BodyMode,
    /// For `@extern("symbol")`: the C function this one stands for.
    extern_symbol: Option<String>,
}

/// Where diagnostics go, with switches to drop them.
struct Sink<'a> {
    out: &'a mut Diagnostics,
    /// Checking a copy of code already checked on its own: errors are kept aside, in case the
    /// copy has an error the original did not.
    quiet: bool,
    /// Checking an expression only to learn its type; it is checked again for real.
    speculative: bool,
    /// The first error dropped while `quiet`.
    suppressed: Option<Diagnostic>,
}

impl Sink<'_> {
    fn push(&mut self, diagnostic: Diagnostic) {
        if self.speculative {
            return;
        }
        if self.quiet {
            if matches!(diagnostic.severity, lang_diag::Severity::Error) && self.suppressed.is_none() {
                self.suppressed = Some(diagnostic);
            }
            return;
        }
        self.out.push(diagnostic);
    }

    fn error_count(&self) -> usize {
        self.out.error_count()
    }
}

struct Checker<'a, 'm> {
    diags: Sink<'a>,
    types: Types,
    prelude_file: FileId,
    structs: Vec<StructInfo>,
    enums: Vec<EnumInfo>,
    traits: Vec<TraitInfo>,
    globals: HashMap<String, (Global, Span)>,
    /// Bare variant names, which may be used without the enum's name when unambiguous.
    variant_names: HashMap<String, Vec<(EnumId, usize)>>,
    /// Indexed by [`FuncId`].
    signatures: Vec<Signature>,
    /// Indexed by [`FuncId`]; `None` for closures, whose HIR is built where they appear.
    bodies: Vec<Option<Body<'m>>>,
    /// The HIR of closures, by [`FuncId`].
    closures: Vec<(FuncId, hir::Function)>,
    /// Methods from `impl Type:` blocks.
    inherent: HashMap<(Ty, String), FuncId>,
    impls: Vec<ImplInfo>,
    impl_index: HashMap<(TraitId, Ty), usize>,
    /// For method calls: which implemented traits provide a method of this name for this type.
    trait_methods: HashMap<(Ty, String), Vec<(TraitId, usize)>>,
    consts: Vec<ConstInfo>,
    struct_items: Vec<(StructId, &'m ast::StructDecl)>,
    enum_items: Vec<(EnumId, &'m ast::EnumDecl)>,
    trait_items: Vec<(TraitId, &'m ast::TraitDecl)>,
    const_items: Vec<(usize, &'m ast::ConstDecl)>,
    fn_items: Vec<(&'m ast::Function, FnSlot)>,
    impl_items: Vec<&'m ast::ImplDecl>,
    builtins: Option<hir::BuiltinTraits>,
    parse_error: Option<StructId>,
    /// The meaning of `Self` in the declaration or body being checked.
    self_ty: Option<Ty>,
    /// Dict key types to check for `Hash` once all implementations are known.
    pending_hashable: Vec<(Ty, Span)>,
    /// Trait methods with a default body, by trait and method index.
    trait_defaults: HashMap<(TraitId, usize), &'m ast::Function>,
    /// Bounds of each type parameter, by its index.
    params: Vec<Vec<TraitId>>,
    /// The names of type parameters in scope, innermost last.
    type_scope: Vec<(String, Ty)>,
    generic_fns: Vec<generics::GenericFn<'m>>,
    generic_types: Vec<generics::GenericType<'m>>,
    impl_templates: Vec<generics::ImplTemplate<'m>>,
    /// For each instance of a generic type: its template and type arguments.
    instance_of: HashMap<Ty, (usize, Vec<Ty>)>,
    /// Instances whose derives and implementations are set up once all `impl` blocks are
    /// known; after that, new instances are set up at once.
    pending_instances: Vec<Ty>,
    generic_setup_done: bool,
    /// Bare variant names of generic enums.
    generic_variant_names: HashMap<String, Vec<(usize, usize)>>,
    /// How the body being checked is checked.
    mode: BodyMode,
    /// The module whose scope `globals` holds.
    current_module: usize,
    /// Each module's name and top-level scope; the current module's scope is in `globals`.
    module_names: Vec<String>,
    module_scopes: Vec<HashMap<String, (Global, Span)>>,
    /// Each module's own declarations, and whether each is `pub`.
    exports: Vec<HashMap<String, (Global, Span, bool)>>,
    /// Which module each source file is.
    file_module: HashMap<FileId, usize>,
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
    /// For a closure: the enclosing function's context, while the closure is checked.
    outer: Option<Box<FnCx>>,
    /// For a closure: the enclosing function's locals it captures, by environment slot.
    captures: Vec<LocalId>,
    /// Optional locals known not to be `None` at this point (flow narrowing): reading one gives
    /// the value inside.
    narrowed: Vec<LocalId>,
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
            outer: None,
            captures: Vec::new(),
            narrowed: Vec::new(),
        }
    }

    /// A context with no locals, for constant values and field defaults.
    fn detached() -> FnCx {
        FnCx::new("", Ty::NONE, None)
    }

    fn add_local(&mut self, name: &str, ty: Ty, decl_span: Span, is_param: bool) -> LocalId {
        let id = LocalId(self.locals.len());
        self.locals.push(hir::Local { name: name.to_string(), ty, boxed: false, capture: None });
        self.info.push(LocalInfo { decl_span, read: false, is_param, hidden: false });
        self.assigned.push(false);
        self.scope.insert(name.to_string(), id);
        id
    }

    /// A hidden local holding an intermediate value, already assigned.
    fn temp(&mut self, ty: Ty, span: Span) -> LocalId {
        let id = LocalId(self.locals.len());
        self.locals.push(hir::Local { name: format!("tmp{}", id.0), ty, boxed: false, capture: None });
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
    fn pattern(p: &ast::Pattern, out: &mut HashMap<String, Span>) {
        match &p.kind {
            ast::PatternKind::Name(id) => {
                out.entry(id.name.clone()).or_insert(id.span);
            }
            ast::PatternKind::Variant { args: Some(args), .. } => args.iter().for_each(|p| pattern(p, out)),
            ast::PatternKind::Tuple(items) | ast::PatternKind::Or(items) => items.iter().for_each(|p| pattern(p, out)),
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
            ast::StmtKind::Def(function) => {
                out.entry(function.name.name.clone()).or_insert(function.name.span);
            }
            ast::StmtKind::For { target: t, body, .. } => {
                target(t, out);
                prescan(body, out);
            }
            ast::StmtKind::Match { cases, .. } => {
                for case in cases {
                    pattern(&case.pattern, out);
                    prescan(&case.body, out);
                }
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

impl<'a, 'm> Checker<'a, 'm> {
    fn new(diags: &'a mut Diagnostics, prelude_file: FileId) -> Self {
        Checker {
            diags: Sink { out: diags, quiet: false, speculative: false, suppressed: None },
            types: Types::new(),
            prelude_file,
            structs: Vec::new(),
            enums: Vec::new(),
            traits: Vec::new(),
            globals: HashMap::new(),
            variant_names: HashMap::new(),
            signatures: Vec::new(),
            bodies: Vec::new(),
            inherent: HashMap::new(),
            impls: Vec::new(),
            impl_index: HashMap::new(),
            trait_methods: HashMap::new(),
            consts: Vec::new(),
            struct_items: Vec::new(),
            enum_items: Vec::new(),
            trait_items: Vec::new(),
            const_items: Vec::new(),
            fn_items: Vec::new(),
            impl_items: Vec::new(),
            builtins: None,
            parse_error: None,
            self_ty: None,
            pending_hashable: Vec::new(),
            trait_defaults: HashMap::new(),
            closures: Vec::new(),
            params: Vec::new(),
            type_scope: Vec::new(),
            generic_fns: Vec::new(),
            generic_types: Vec::new(),
            impl_templates: Vec::new(),
            instance_of: HashMap::new(),
            pending_instances: Vec::new(),
            generic_setup_done: false,
            generic_variant_names: HashMap::new(),
            mode: BodyMode::Normal,
            current_module: 0,
            module_names: Vec::new(),
            module_scopes: Vec::new(),
            exports: Vec::new(),
            file_module: HashMap::new(),
        }
    }

    fn into_program(self, module_name: &str, functions: Vec<hir::Function>, entry: FuncId) -> hir::Program {
        let fields =
            |fields: &[FieldInfo]| fields.iter().map(|f| hir::FieldDef { name: f.name.clone(), ty: f.ty }).collect();
        // Instances of generic types print with the template's name: `Box(value=1)`.
        let base_name = |ty: Ty, name: &str| match self.instance_of.get(&ty) {
            Some((template, _)) => self.generic_types[*template].name.clone(),
            None => name.to_string(),
        };
        let structs = self
            .structs
            .iter()
            .map(|s| hir::StructDef {
                name: base_name(s.ty, &s.name),
                ty: s.ty,
                fields: fields(&s.fields),
                generic: s.generic,
            })
            .collect();
        let enums = self
            .enums
            .iter()
            .map(|e| hir::EnumDef {
                name: base_name(e.ty, &e.name),
                ty: e.ty,
                variants: e
                    .variants
                    .iter()
                    .map(|v| hir::VariantDef { name: v.name.clone(), fields: fields(&v.fields) })
                    .collect(),
                generic: e.generic,
            })
            .collect();
        let traits = self
            .traits
            .iter()
            .map(|t| hir::TraitDef {
                name: t.name.clone(),
                supertraits: t.supertraits.clone(),
                methods: t
                    .methods
                    .iter()
                    .map(|m| hir::TraitMethodDef {
                        name: m.name.clone(),
                        params: m.params.iter().map(|p| p.ty).collect(),
                        ret: m.ret,
                    })
                    .collect(),
            })
            .collect();
        let impls = self
            .impls
            .iter()
            .map(|i| hir::ImplDef { trait_id: i.trait_id, ty: i.ty, methods: i.methods.clone(), derived: i.derived })
            .collect();
        hir::Program {
            module: module_name.to_string(),
            types: self.types,
            structs,
            enums,
            traits,
            impls,
            impl_index: self.impl_index,
            functions,
            entry,
            builtins: self.builtins.expect("the prelude defines the built-in traits"),
            parse_error: self.parse_error.expect("the prelude defines ParseError"),
        }
    }

    fn error(&mut self, diagnostic: Diagnostic) {
        self.diags.push(diagnostic);
    }

    fn display(&self, ty: Ty) -> String {
        self.types.display(ty)
    }

    fn builtin(&self) -> hir::BuiltinTraits {
        self.builtins.expect("built-in traits are known")
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
            ast::TypeExprKind::Func { params, ret } => {
                let params: Vec<Ty> = params.iter().map(|p| self.resolve_value_type(p)).collect();
                let ret = ret.as_ref().map_or(Ty::NONE, |r| self.resolve_type(r));
                self.types.func(params, ret)
            }
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

    fn no_type_args(&mut self, name: &str, args: &[ast::TypeExpr], span: Span) -> bool {
        if args.is_empty() {
            return true;
        }
        self.error(Diagnostic::error(
            codes::UNEXPECTED_TYPE_ARGS,
            format!("the type `{name}` does not take type arguments"),
            span,
            "remove the `[...]`",
        ));
        false
    }

    fn resolve_named_type(&mut self, name: &str, args: &[ast::TypeExpr], span: Span) -> Ty {
        let arity = |expected: usize| args.len() == expected;
        if let Some(ty) = self.type_param(name) {
            if !self.no_type_args(name, args, span) {
                return Ty::ERROR;
            }
            return ty;
        }
        match name {
            "int" | "bool" | "str" | "float" => {
                if !self.no_type_args(name, args, span) {
                    return Ty::ERROR;
                }
                match name {
                    "int" => Ty::INT,
                    "bool" => Ty::BOOL,
                    "str" => Ty::STR,
                    _ => Ty::FLOAT,
                }
            }
            "i64" | "f64" => {
                if !self.no_type_args(name, args, span) {
                    return Ty::ERROR;
                }
                if name == "i64" { Ty::INT } else { Ty::FLOAT }
            }
            sized if crate::types::SizedKind::from_name(sized).is_some() => {
                if !self.no_type_args(name, args, span) {
                    return Ty::ERROR;
                }
                let kind = crate::types::SizedKind::from_name(sized).expect("checked");
                self.types.sized(kind)
            }
            "Self" => match self.self_ty {
                Some(ty) => ty,
                None => {
                    self.error(Diagnostic::error(
                        codes::UNKNOWN_TYPE,
                        "`Self` is only available inside traits and `impl` blocks",
                        span,
                        "no `Self` here",
                    ));
                    Ty::ERROR
                }
            },
            "list" if arity(1) => {
                let elem = self.resolve_value_type(&args[0]);
                self.types.list(elem)
            }
            "dict" if arity(2) => {
                let key = self.resolve_value_type(&args[0]);
                let value = self.resolve_value_type(&args[1]);
                self.pending_hashable.push((key, args[0].span));
                self.types.dict(key, value)
            }
            "tuple" if !args.is_empty() => {
                let items = args.iter().map(|a| self.resolve_value_type(a)).collect();
                self.types.tuple(items)
            }
            "Result" if arity(1) || arity(2) => {
                let ok = self.resolve_type(&args[0]);
                let err = if args.len() == 2 {
                    self.resolve_value_type(&args[1])
                } else {
                    let error = self.builtin().error;
                    self.types.dyn_(error)
                };
                self.types.result(ok, err)
            }
            "list" | "dict" | "tuple" | "Result" => {
                let expected = match name {
                    "list" => "`list[T]`, with one element type",
                    "dict" => "`dict[K, V]`, with a key type and a value type",
                    "Result" => "`Result[T]` or `Result[T, E]`",
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
            "Option" => {
                self.error(
                    Diagnostic::error(codes::UNKNOWN_TYPE, "write an optional type with `?`", span, "not a type name")
                        .with_help("`Option[int]` is written `int?`"),
                );
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
            other => match self.type_global(other, span) {
                Some(Global::Struct(id)) => {
                    self.no_type_args(other, args, span);
                    self.structs[id.0].ty
                }
                Some(Global::Enum(id)) => {
                    self.no_type_args(other, args, span);
                    self.enums[id.0].ty
                }
                Some(Global::GenericStruct(index) | Global::GenericEnum(index)) => {
                    let count = self.generic_types[index].params.len();
                    if args.len() != count {
                        self.error(
                            Diagnostic::error(
                                codes::UNEXPECTED_TYPE_ARGS,
                                format!("`{other}` takes {}", plural(count, "type argument")),
                                span,
                                format!("expected {}", plural(count, "type argument")),
                            )
                            .with_help(format!("for example `{other}[int]`")),
                        );
                        return Ty::ERROR;
                    }
                    let types: Vec<Ty> = args.iter().map(|a| self.resolve_value_type(a)).collect();
                    if types.contains(&Ty::ERROR) {
                        return Ty::ERROR;
                    }
                    let params = self.generic_types[index].params.clone();
                    if !self.check_bounds(other, &params, &types, span) {
                        return Ty::ERROR;
                    }
                    self.instantiate_type(index, types, span)
                }
                Some(Global::Trait(id)) => {
                    self.no_type_args(other, args, span);
                    if let Err(reason) = self.object_safety(id) {
                        self.error(
                            Diagnostic::error(
                                codes::NOT_OBJECT_SAFE,
                                format!("the trait `{other}` cannot be used as a type"),
                                span,
                                reason,
                            )
                            .with_help("only traits whose methods all take `self` and never mention `Self` can be used as types"),
                        );
                        return Ty::ERROR;
                    }
                    self.types.dyn_(id)
                }
                _ => {
                    self.unknown_type(other, span);
                    Ty::ERROR
                }
            },
        }
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
                let user = self.globals.keys().map(String::as_str);
                closest(name, KNOWN_TYPES.iter().copied().chain(user)).map(|s| format!("did you mean `{s}`?"))
            }
        };
        let mut diagnostic =
            Diagnostic::error(codes::UNKNOWN_TYPE, format!("unknown type `{name}`"), span, "not a type");
        if let Some(help) = help {
            diagnostic = diagnostic.with_help(help);
        }
        self.error(diagnostic);
    }

    fn unhashable(&mut self, key: Ty, span: Span) {
        let shown = self.display(key);
        let help = if matches!(self.types.kind(key), TyKind::Struct(_) | TyKind::Enum(_)) {
            format!("add `@derive(Eq, Hash)` to `{shown}` to use it as a key")
        } else {
            "dict keys can be `int`, `str`, `bool`, tuples of those, or types that implement `Hash`".to_string()
        };
        self.error(
            Diagnostic::error(
                codes::UNHASHABLE_KEY,
                format!("`{shown}` cannot be used as a dict key"),
                span,
                "not hashable",
            )
            .with_help(help),
        );
    }

    fn check_pending_hashable(&mut self) {
        for (key, span) in std::mem::take(&mut self.pending_hashable) {
            if !self.hashable(key) {
                self.unhashable(key, span);
            }
        }
    }

    /// Replace `Self` in a trait's signature types with the implementing type.
    fn subst_self(&mut self, ty: Ty, with: Ty) -> Ty {
        match self.types.kind(ty).clone() {
            TyKind::SelfType(_) => with,
            TyKind::List(t) => {
                let t = self.subst_self(t, with);
                self.types.list(t)
            }
            TyKind::Option(t) => {
                let t = self.subst_self(t, with);
                self.types.option(t)
            }
            TyKind::Dict(k, v) => {
                let (k, v) = (self.subst_self(k, with), self.subst_self(v, with));
                self.types.dict(k, v)
            }
            TyKind::Result(k, v) => {
                let (k, v) = (self.subst_self(k, with), self.subst_self(v, with));
                self.types.result(k, v)
            }
            TyKind::Tuple(items) => {
                let items = items.into_iter().map(|t| self.subst_self(t, with)).collect();
                self.types.tuple(items)
            }
            TyKind::Func(params, ret) => {
                let params = params.into_iter().map(|t| self.subst_self(t, with)).collect();
                let ret = self.subst_self(ret, with);
                self.types.func(params, ret)
            }
            _ => ty,
        }
    }

    fn mentions_self(&self, ty: Ty) -> bool {
        match self.types.kind(ty) {
            TyKind::SelfType(_) => true,
            TyKind::List(t) | TyKind::Option(t) => self.mentions_self(*t),
            TyKind::Dict(k, v) | TyKind::Result(k, v) => self.mentions_self(*k) || self.mentions_self(*v),
            TyKind::Tuple(items) => items.iter().any(|t| self.mentions_self(*t)),
            TyKind::Func(params, ret) => params.iter().any(|t| self.mentions_self(*t)) || self.mentions_self(*ret),
            _ => false,
        }
    }

    // ---- traits -------------------------------------------------------------------------

    /// Whether a trait can be used as a type, or why not.
    fn object_safety(&self, id: TraitId) -> Result<(), String> {
        for t in self.trait_closure(id) {
            for method in &self.traits[t.0].methods {
                if !method.has_self {
                    return Err(format!("method `{}` does not take `self`", method.name));
                }
                if method.params.iter().any(|p| self.mentions_self(p.ty)) || self.mentions_self(method.ret) {
                    return Err(format!("method `{}` mentions `Self`", method.name));
                }
            }
        }
        Ok(())
    }

    /// A trait and every trait it requires, directly or not, starting with itself.
    fn trait_closure(&self, id: TraitId) -> Vec<TraitId> {
        let mut out = vec![id];
        let mut i = 0;
        while i < out.len() {
            for &s in &self.traits[out[i].0].supertraits {
                if !out.contains(&s) {
                    out.push(s);
                }
            }
            i += 1;
        }
        out
    }

    /// The vtable slots leading from trait `from` to its required trait `to`.
    fn upcast_path(&self, from: TraitId, to: TraitId) -> Option<Vec<usize>> {
        if from == to {
            return Some(Vec::new());
        }
        for (slot, &s) in self.traits[from.0].supertraits.iter().enumerate() {
            if let Some(mut rest) = self.upcast_path(s, to) {
                rest.insert(0, slot);
                return Some(rest);
            }
        }
        None
    }

    /// Find a method on a trait or its required traits: the declaring trait, the path to it,
    /// and the method's index.
    fn find_trait_method(&self, id: TraitId, name: &str) -> Option<(TraitId, Vec<usize>, usize)> {
        for t in self.trait_closure(id) {
            if let Some(index) = self.traits[t.0].methods.iter().position(|m| m.name == name) {
                return Some((t, self.upcast_path(id, t)?, index));
            }
        }
        None
    }

    fn implements(&self, ty: Ty, trait_id: TraitId) -> bool {
        match self.types.kind(ty) {
            TyKind::Dyn(t) | TyKind::SelfType(t) => self.trait_closure(*t).contains(&trait_id),
            TyKind::Param(index) => {
                self.params[*index as usize].iter().any(|bound| self.trait_closure(*bound).contains(&trait_id))
            }
            TyKind::Error => true,
            _ => self.impl_index.contains_key(&(trait_id, ty)),
        }
    }

    // ---- what operations a type supports ------------------------------------------------

    /// Can be printed, formatted and converted with `str()`.
    fn displayable(&self, ty: Ty) -> bool {
        let b = self.builtin();
        match self.types.kind(ty) {
            TyKind::Struct(_) | TyKind::Enum(_) | TyKind::Param(_) => self.implements(ty, b.display),
            TyKind::Func(..) => false,
            TyKind::Dyn(t) | TyKind::SelfType(t) => {
                let closure = self.trait_closure(*t);
                closure.contains(&b.display) || closure.contains(&b.error)
            }
            TyKind::List(t) | TyKind::Option(t) => self.displayable(*t),
            TyKind::Dict(k, v) | TyKind::Result(k, v) => self.displayable(*k) && self.displayable(*v),
            TyKind::Tuple(items) => items.iter().all(|t| self.displayable(*t)),
            _ => true,
        }
    }

    /// Supports `==`.
    fn equatable(&self, ty: Ty) -> bool {
        match self.types.kind(ty) {
            TyKind::Struct(_) | TyKind::Enum(_) | TyKind::Param(_) => self.implements(ty, self.builtin().eq),
            TyKind::Error | TyKind::None | TyKind::Bool | TyKind::Int | TyKind::Float | TyKind::Str => true,
            TyKind::Sized(_) => true,
            TyKind::List(t) | TyKind::Option(t) => self.equatable(*t),
            TyKind::Result(k, v) => self.equatable(*k) && self.equatable(*v),
            TyKind::Tuple(items) => items.iter().all(|t| self.equatable(*t)),
            TyKind::Dyn(t) | TyKind::SelfType(t) => self.trait_closure(*t).contains(&self.builtin().eq),
            TyKind::Dict(..) | TyKind::Func(..) => false,
        }
    }

    /// Usable as a dict key.
    fn hashable(&self, ty: Ty) -> bool {
        match self.types.kind(ty) {
            TyKind::Error | TyKind::Int | TyKind::Bool | TyKind::Str => true,
            TyKind::Sized(kind) => !kind.is_float(),
            TyKind::Struct(_) | TyKind::Enum(_) | TyKind::Param(_) => self.implements(ty, self.builtin().hash),
            TyKind::Tuple(items) => items.iter().all(|t| self.hashable(*t)),
            _ => false,
        }
    }

    /// Supports `<` and sorting: numbers, strings, booleans, types implementing `Ord`, and
    /// tuples and lists of those, compared element by element.
    fn orderable(&self, ty: Ty) -> bool {
        match self.types.kind(ty) {
            TyKind::Error | TyKind::Bool | TyKind::Int | TyKind::Float | TyKind::Str | TyKind::Sized(_) => true,
            TyKind::Struct(_) | TyKind::Enum(_) | TyKind::Param(_) => self.implements(ty, self.builtin().ord),
            TyKind::Tuple(items) => items.iter().all(|t| self.orderable(*t)),
            TyKind::List(t) => self.orderable(*t),
            _ => false,
        }
    }

    // ---- bodies -------------------------------------------------------------------------

    /// Check function `id`, or `None` for a closure, whose HIR is made where it appears.
    fn function(&mut self, id: FuncId) -> Option<hir::Function> {
        let body = self.bodies[id.0].as_ref()?;
        let (ast, self_ty, bindings, mode) = (body.ast, body.self_ty, body.bindings.clone(), body.mode);
        let extern_symbol = body.extern_symbol.clone();
        let signature = self.signatures[id.0].clone();
        self.enter_module(self.module_of(ast.name.span));
        if let Some(symbol) = extern_symbol {
            return Some(self.extern_function(&signature, symbol));
        }
        let generic = matches!(mode, BodyMode::GenericCheck | BodyMode::Skip)
            || bindings.iter().any(|(_, t)| self.types.contains_param(*t))
            || self_ty.is_some_and(|t| self.types.contains_param(t));
        if mode == BodyMode::Skip || !ast.has_body {
            // Signatures only; a bodiless function that is not a valid `@extern` was reported.
            return Some(hir::Function {
                name: signature.name,
                params: Vec::new(),
                locals: Vec::new(),
                ret: signature.ret,
                body: Vec::new(),
                generic: true,
                closure: false,
                extern_symbol: None,
            });
        }
        let saved_mode = std::mem::replace(&mut self.mode, mode);
        let mut function =
            self.with_scope(bindings, mode == BodyMode::Quiet, |this| this.check_body(&signature, ast, self_ty));
        self.mode = saved_mode;
        function.generic = generic;
        Some(function)
    }

    fn check_body(&mut self, signature: &Signature, function: &ast::Function, self_ty: Option<Ty>) -> hir::Function {
        let saved_self = std::mem::replace(&mut self.self_ty, self_ty);
        let (name, ret, ret_span) = (signature.name.clone(), signature.ret, signature.ret_span);
        let mut cx = FnCx::new(&name, ret, ret_span);
        if let Some((ty, span)) = signature.receiver {
            let local = cx.add_local("self", ty, span, true);
            cx.set_assigned(local);
        }
        for param in &signature.params {
            let local = cx.add_local(&param.name, param.ty, param.hint_span, true);
            cx.set_assigned(local);
        }
        let param_count = cx.locals.len();
        prescan(&function.body, &mut cx.first_assignment);

        let (mut body, diverges) = self.block(&mut cx, &function.body);
        // A function returning `Result[None]` succeeds by reaching its end.
        self.finish_body(&cx, &mut body, diverges, function.name.span);
        self.warn_unused(&cx);
        self.self_ty = saved_self;
        hir::Function {
            name,
            params: (0..param_count).map(LocalId).collect(),
            locals: cx.locals,
            ret,
            body,
            generic: false,
            closure: false,
            extern_symbol: None,
        }
    }

    // ---- shared helpers -----------------------------------------------------------------

    /// Whether `ret` is `Result[None, E]`, where reaching the end or a bare `return` means success.
    fn returns_ok_none(&self, ret: Ty) -> bool {
        self.types.result_parts(ret).is_some_and(|(ok, _)| ok == Ty::NONE)
    }

    fn ok_none(&self, ret: Ty, span: Span) -> hir::Expr {
        let none = hir::Expr { kind: hir::ExprKind::None, ty: Ty::NONE, span };
        hir::Expr { kind: hir::ExprKind::Ok(Box::new(none)), ty: ret, span }
    }

    /// What a top-level name is, for error messages.
    fn global_kind(&self, name: &str) -> Option<&'static str> {
        match self.globals.get(name) {
            Some((Global::Function(_), _)) => Some("a function"),
            Some((Global::Struct(_), _)) => Some("a struct"),
            Some((Global::Enum(_), _)) => Some("an enum"),
            Some((Global::Trait(_), _)) => Some("a trait"),
            Some((Global::Const(_), _)) => Some("a constant"),
            Some((Global::GenericFunction(_), _)) => Some("a function"),
            Some((Global::GenericStruct(_), _)) => Some("a struct"),
            Some((Global::GenericEnum(_), _)) => Some("an enum"),
            Some((Global::Module(_), _)) => Some("a module"),
            Some((Global::Missing, _)) => Some("unknown"),
            // Built-in functions can be shadowed by local variables, as in Python.
            None if BUILTIN_CONSTRUCTORS.contains(&name) => Some("a built-in function"),
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
mod tests;
