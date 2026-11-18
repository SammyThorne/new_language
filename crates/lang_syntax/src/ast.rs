//! The syntax tree. Every node carries the span of the source it came from.
//!
//! Constructs the parser recognises but the compiler does not support yet are reported as errors
//! during parsing and never reach the tree.

use lang_diag::Span;

#[derive(Debug)]
pub struct Module {
    pub items: Vec<Item>,
    /// A zero-width span at the start of the file, for diagnostics about the file as a whole.
    pub start: Span,
}

#[derive(Debug)]
pub enum Item {
    Function(Function),
    Struct(StructDecl),
    Impl(ImplDecl),
    Const(ConstDecl),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visibility {
    /// No keyword: private to the file for top-level declarations, visible with the type for
    /// members.
    Default,
    Pub,
    Priv,
}

#[derive(Debug)]
pub struct Function {
    pub visibility: Visibility,
    pub name: Ident,
    /// Set for methods whose first parameter is `self`; holds the span of `self`.
    pub self_param: Option<Span>,
    /// Parameters other than `self`.
    pub params: Vec<Param>,
    pub ret: Option<TypeExpr>,
    pub body: Vec<Stmt>,
}

#[derive(Debug)]
pub struct StructDecl {
    pub visibility: Visibility,
    pub name: Ident,
    pub fields: Vec<FieldDecl>,
}

#[derive(Debug)]
pub struct FieldDecl {
    pub visibility: Visibility,
    pub name: Ident,
    pub ty: TypeExpr,
    /// Evaluated afresh for every construction that does not supply the field.
    pub default: Option<Expr>,
}

#[derive(Debug)]
pub struct ImplDecl {
    /// The type the methods belong to.
    pub target: Ident,
    pub methods: Vec<Function>,
}

#[derive(Debug)]
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

#[derive(Debug)]
pub struct Param {
    pub name: Ident,
    /// `None` when the hint is missing. That is a type error, not a syntax error, so the
    /// parser accepts it and the type checker reports it.
    pub ty: Option<TypeExpr>,
}

#[derive(Debug)]
pub struct TypeExpr {
    pub kind: TypeExprKind,
    pub span: Span,
}

#[derive(Debug)]
pub enum TypeExprKind {
    /// `int`, `list[int]`, `dict[str, int]`, `User`.
    Named { name: String, args: Vec<TypeExpr> },
    /// `T?` or `T | None`.
    Optional(Box<TypeExpr>),
    /// `None`, as in `-> None`.
    None,
}

#[derive(Debug)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Debug)]
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
    Break,
    Continue,
}

#[derive(Debug)]
pub struct Target {
    pub kind: TargetKind,
    pub span: Span,
}

#[derive(Debug)]
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

#[derive(Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Debug)]
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
    IfElse {
        cond: Box<Expr>,
        then: Box<Expr>,
        otherwise: Box<Expr>,
    },
}

#[derive(Debug)]
pub struct Arg {
    /// Set for keyword arguments: `name=value`.
    pub name: Option<Ident>,
    pub value: Expr,
}

#[derive(Debug)]
pub enum FStringPart {
    Literal(String),
    Expr { expr: Expr, spec: Option<(String, Span)> },
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
