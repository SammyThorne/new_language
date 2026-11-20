//! The syntax tree. Every node carries the span of the source it came from.
//!
//! Constructs the parser recognises but the compiler does not support yet are reported as errors
//! during parsing and never reach the tree.

use lang_diag::Span;

#[derive(Debug, Clone)]
pub struct Module {
    pub items: Vec<Item>,
    /// A zero-width span at the start of the file, for diagnostics about the file as a whole.
    pub start: Span,
}

#[derive(Debug, Clone)]
pub enum Item {
    Function(Function),
    Struct(StructDecl),
    Enum(EnumDecl),
    Trait(TraitDecl),
    Impl(ImplDecl),
    Const(ConstDecl),
    Import(ImportDecl),
    TypeAlias(TypeAliasDecl),
}

/// `type Handler = def(Request) -> Response`: another name for a type.
#[derive(Debug, Clone)]
pub struct TypeAliasDecl {
    pub visibility: Visibility,
    pub name: Ident,
    pub ty: TypeExpr,
}

/// `import a.b`, `import a.b as c`, or `from a.b import x, y as z`.
#[derive(Debug, Clone)]
pub struct ImportDecl {
    /// The module's dotted name, one identifier per part.
    pub path: Vec<Ident>,
    /// `as c` after `import a.b`.
    pub alias: Option<Ident>,
    /// For `from ... import`: each imported name and its optional `as` name.
    pub names: Option<Vec<(Ident, Option<Ident>)>>,
    pub span: Span,
}

/// `@name`, `@name(arg, ...)` or `@module.name(...)` before a declaration. A fixed,
/// compiler-known set.
#[derive(Debug, Clone)]
pub struct Annotation {
    /// The name, with dots for qualified names such as `http.get`.
    pub name: Ident,
    pub args: Vec<AnnotationArg>,
    pub span: Span,
}

/// One argument of an annotation: a name, a string, or `keyword=value`.
#[derive(Debug, Clone)]
pub struct AnnotationArg {
    pub keyword: Option<Ident>,
    pub value: AnnotationValue,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum AnnotationValue {
    Name(Ident),
    Str(String),
    Int(i64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visibility {
    /// No keyword: private to the file for top-level declarations, visible with the type for
    /// members.
    Default,
    Pub,
    Priv,
}

/// A type parameter: `T` or `T: Ord + Display`.
#[derive(Debug, Clone)]
pub struct GenericParam {
    pub name: Ident,
    pub bounds: Vec<Ident>,
}

#[derive(Debug, Clone)]
pub struct Function {
    pub annotations: Vec<Annotation>,
    pub visibility: Visibility,
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    /// Set for methods whose first parameter is `self`; holds the span of `self`.
    pub self_param: Option<Span>,
    /// Parameters other than `self`.
    pub params: Vec<Param>,
    pub ret: Option<TypeExpr>,
    pub body: Vec<Stmt>,
    /// False for a trait method declared without a body (a required method), and for a C
    /// function bound with `@extern`.
    pub has_body: bool,
}

#[derive(Debug, Clone)]
pub struct StructDecl {
    pub annotations: Vec<Annotation>,
    pub visibility: Visibility,
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub fields: Vec<FieldDecl>,
}

#[derive(Debug, Clone)]
pub struct FieldDecl {
    /// `@json(name="...")` lines before the field.
    pub annotations: Vec<Annotation>,
    pub visibility: Visibility,
    pub name: Ident,
    pub ty: TypeExpr,
    /// Evaluated afresh for every construction that does not supply the field.
    pub default: Option<Expr>,
}

#[derive(Debug, Clone)]
pub struct EnumDecl {
    pub annotations: Vec<Annotation>,
    pub visibility: Visibility,
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub variants: Vec<VariantDecl>,
}

#[derive(Debug, Clone)]
pub struct VariantDecl {
    pub name: Ident,
    /// Payload fields; empty for a variant without a payload.
    pub fields: Vec<FieldDecl>,
}

#[derive(Debug, Clone)]
pub struct TraitDecl {
    pub visibility: Visibility,
    pub name: Ident,
    /// Traits every implementor must also implement: `trait A(B, C):`.
    pub supertraits: Vec<Ident>,
    /// Required methods have `has_body == false`; the others are defaults.
    pub methods: Vec<Function>,
}

#[derive(Debug, Clone)]
pub struct ImplDecl {
    /// `impl[T] ...`: type parameters for implementing a generic type.
    pub generics: Vec<GenericParam>,
    /// `impl Trait for Type:`; `None` for inherent methods, `impl Type:`.
    pub trait_name: Option<Ident>,
    /// The type the methods belong to.
    pub target: Ident,
    /// Type arguments of the target: `T` in `impl[T] Stack[T]:`.
    pub target_args: Vec<TypeExpr>,
    pub methods: Vec<Function>,
}

#[derive(Debug, Clone)]
pub struct ConstDecl {
    pub visibility: Visibility,
    pub name: Ident,
    pub ty: Option<TypeExpr>,
    pub value: Expr,
}

#[derive(Clone, Debug)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: Ident,
    /// `None` when the hint is missing. That is a type error, not a syntax error, so the
    /// parser accepts it and the type checker reports it.
    pub ty: Option<TypeExpr>,
    /// Evaluated at each call that leaves the argument out.
    pub default: Option<Expr>,
}

#[derive(Debug, Clone)]
pub struct TypeExpr {
    pub kind: TypeExprKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum TypeExprKind {
    /// `int`, `list[int]`, `dict[str, int]`, `User`.
    Named { name: String, args: Vec<TypeExpr> },
    /// `T?` or `T | None`.
    Optional(Box<TypeExpr>),
    /// `None`, as in `-> None`.
    None,
    /// `def(int, str) -> bool`; no return type means `None`.
    Func { params: Vec<TypeExpr>, ret: Option<Box<TypeExpr>> },
}

#[derive(Debug, Clone)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum StmtKind {
    Expr(Expr),
    /// `target = value`, or `name: type = value`. The first assignment to a name declares it.
    Assign {
        target: Target,
        ty: Option<TypeExpr>,
        value: Expr,
    },
    /// `target += value` and the other augmented operators.
    AugAssign {
        target: Target,
        op: BinaryOp,
        value: Expr,
    },
    Return(Option<Expr>),
    /// `if`/`elif` branches in order, then the optional `else` block.
    If {
        branches: Vec<(Expr, Vec<Stmt>)>,
        otherwise: Option<Vec<Stmt>>,
    },
    While {
        cond: Expr,
        body: Vec<Stmt>,
    },
    For {
        target: Target,
        iter: Expr,
        body: Vec<Stmt>,
    },
    Match {
        subject: Expr,
        cases: Vec<Case>,
    },
    /// A function defined inside another, which can use the enclosing function's variables.
    Def(Box<Function>),
    /// `with value as name:`; the value's `exit` method runs however the block is left.
    With {
        value: Expr,
        target: Option<Ident>,
        body: Vec<Stmt>,
    },
    /// `del xs[i]` or `del d[key]`: remove an element of a list or an entry of a dict. The
    /// checker reports any other operand.
    Del(Expr),
    Break,
    Continue,
    /// `pass`: does nothing; fills a block that needs no statements.
    Pass,
}

#[derive(Debug, Clone)]
pub struct Case {
    pub pattern: Pattern,
    pub guard: Option<Expr>,
    pub body: Vec<Stmt>,
}

#[derive(Debug, Clone)]
pub struct Pattern {
    pub kind: PatternKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum PatternKind {
    /// `_`
    Wild,
    /// A bare name: a variant without payload if the subject's type has one by that name
    /// (or `None`), otherwise a new binding.
    Name(Ident),
    /// A literal number, string or boolean, possibly negated.
    Literal(Expr),
    /// `Variant(p, ...)`, `Enum.Variant(p, ...)` or `Enum.Variant`. `path` holds the names.
    Variant {
        path: Vec<Ident>,
        args: Option<Vec<Pattern>>,
    },
    Tuple(Vec<Pattern>),
    /// `p | q`
    Or(Vec<Pattern>),
}

#[derive(Debug, Clone)]
pub struct Target {
    pub kind: TargetKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum TargetKind {
    Name(Ident),
    Attr {
        object: Expr,
        field: Ident,
    },
    Index {
        object: Expr,
        index: Expr,
    },
    /// `a, b = ...`
    Tuple(Vec<Target>),
}

#[derive(Debug, Clone)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    /// The literal's magnitude; a leading `-` is a separate unary operator.
    Int(u64),
    Float(f64),
    Str(String),
    FString(Vec<FStringPart>),
    Bool(bool),
    None,
    /// A name, including `self`.
    Name(String),
    Call {
        callee: Box<Expr>,
        args: Vec<Arg>,
    },
    Attr {
        object: Box<Expr>,
        name: Ident,
    },
    Index {
        object: Box<Expr>,
        index: Box<Expr>,
    },
    Slice {
        object: Box<Expr>,
        start: Option<Box<Expr>>,
        stop: Option<Box<Expr>>,
    },
    List(Vec<Expr>),
    Dict(Vec<(Expr, Expr)>),
    Tuple(Vec<Expr>),
    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// `then if cond else otherwise`
    /// `match subject:` with one `case pattern: value` per line, as the whole right-hand side
    /// of an assignment or a `return`. Each case's body is a single expression statement.
    Match {
        subject: Box<Expr>,
        cases: Vec<Case>,
    },
    IfElse {
        cond: Box<Expr>,
        then: Box<Expr>,
        otherwise: Box<Expr>,
    },
    /// `expr?`
    Try(Box<Expr>),
    /// `spawn f(args)`: run a call in a new task. The parser checks that the operand is a call.
    Spawn(Box<Expr>),
    /// `lambda x, y: body`
    Lambda {
        params: Vec<Ident>,
        body: Box<Expr>,
    },
    /// `[element for target in iter if condition]`
    ListComp {
        element: Box<Expr>,
        clauses: Vec<CompClause>,
    },
    /// `{key: value for target in iter if condition}`
    DictComp {
        key: Box<Expr>,
        value: Box<Expr>,
        clauses: Vec<CompClause>,
    },
}

/// One `for ... in ... if ...` clause of a comprehension.
#[derive(Debug, Clone)]
pub struct CompClause {
    pub target: Target,
    pub iter: Expr,
    pub conditions: Vec<Expr>,
}

#[derive(Debug, Clone)]
pub struct Arg {
    /// Set for keyword arguments: `name=value`.
    pub name: Option<Ident>,
    pub value: Expr,
}

#[derive(Debug, Clone)]
pub enum FStringPart {
    Literal(String),
    /// `{expr}`, `{expr:spec}`, `{expr!r}` or `{expr!r:spec}`. `conversion` is `r` or `s`.
    Expr {
        expr: Expr,
        spec: Option<(String, Span)>,
        conversion: Option<char>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Plus,
    Not,
    Invert,
}

impl UnaryOp {
    pub fn symbol(self) -> &'static str {
        match self {
            UnaryOp::Neg => "-",
            UnaryOp::Plus => "+",
            UnaryOp::Not => "not",
            UnaryOp::Invert => "~",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOp {
    Or,
    And,
    Eq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,
    In,
    NotIn,
    Is,
    IsNot,
    BitOr,
    BitXor,
    BitAnd,
    Shl,
    Shr,
    Add,
    Sub,
    Mul,
    Div,
    FloorDiv,
    Mod,
    Pow,
}

impl BinaryOp {
    pub fn symbol(self) -> &'static str {
        use BinaryOp::*;
        match self {
            Or => "or",
            And => "and",
            Eq => "==",
            NotEq => "!=",
            Lt => "<",
            Le => "<=",
            Gt => ">",
            Ge => ">=",
            In => "in",
            NotIn => "not in",
            Is => "is",
            IsNot => "is not",
            BitOr => "|",
            BitXor => "^",
            BitAnd => "&",
            Shl => "<<",
            Shr => ">>",
            Add => "+",
            Sub => "-",
            Mul => "*",
            Div => "/",
            FloorDiv => "//",
            Mod => "%",
            Pow => "**",
        }
    }

    pub fn is_comparison(self) -> bool {
        use BinaryOp::*;
        matches!(self, Eq | NotEq | Lt | Le | Gt | Ge | In | NotIn | Is | IsNot)
    }
}

impl Expr {
    /// Whether this is an integer literal, possibly negated. Such literals adapt to `float` when
    /// used with one (docs/08, D17).
    pub fn is_int_literal(&self) -> bool {
        match &self.kind {
            ExprKind::Int(_) => true,
            ExprKind::Unary { op: UnaryOp::Neg | UnaryOp::Plus, operand } => operand.is_int_literal(),
            _ => false,
        }
    }

    pub fn is_none_literal(&self) -> bool {
        matches!(self.kind, ExprKind::None)
    }
}
