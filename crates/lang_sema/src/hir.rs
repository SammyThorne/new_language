//! HIR: the typed program handed to code generation.
//!
//! Every name is resolved, every expression has a type, operators are specialised to their
//! operand types, and sugar (augmented assignment, tuple unpacking, `elif`, field defaults) is
//! gone. HIR only exists for programs without errors, so it never contains [`Ty::ERROR`].

use crate::types::{StructId, Ty, Types};
use lang_diag::Span;

#[derive(Debug)]
pub struct Program {
    /// Module name, used to build symbol names. Taken from the file name.
    pub module: String,
    pub types: Types,
    /// Indexed by [`StructId`].
    pub structs: Vec<StructDef>,
    /// Functions and methods, indexed by [`FuncId`].
    pub functions: Vec<Function>,
    /// The program's `main`.
    pub entry: FuncId,
}

#[derive(Debug)]
pub struct StructDef {
    pub name: String,
    pub ty: Ty,
    pub fields: Vec<FieldDef>,
}

#[derive(Debug)]
pub struct FieldDef {
    pub name: String,
    pub ty: Ty,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FuncId(pub usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LocalId(pub usize);

#[derive(Debug)]
pub struct Function {
    /// `name` for functions, `Type.name` for methods.
    pub name: String,
    /// The first `params.len()` entries of `locals` are the parameters, in order (`self`
    /// first, for methods).
    pub params: Vec<LocalId>,
    pub locals: Vec<Local>,
    pub ret: Ty,
    pub body: Block,
}

#[derive(Debug)]
pub struct Local {
    pub name: String,
    pub ty: Ty,
}

pub type Block = Vec<Stmt>;

#[derive(Clone, Debug)]
pub enum Stmt {
    /// Covers declaring first assignments and later reassignments.
    Assign {
        place: Place,
        value: Expr,
    },
    Expr(Expr),
    Return(Option<Expr>),
    If {
        cond: Expr,
        then: Block,
        otherwise: Block,
    },
    While {
        cond: Expr,
        body: Block,
    },
    For {
        target: LoopTarget,
        source: LoopSource,
        body: Block,
    },
    Break,
    Continue,
}

#[derive(Clone, Debug)]
pub enum Place {
    Local(LocalId),
    Field {
        object: Expr,
        index: usize,
    },
    /// An element of a list (bounds-checked) or a dict (inserted if absent).
    Index {
        container: Expr,
        index: Expr,
        span: Span,
    },
}

#[derive(Clone, Debug)]
pub enum LoopTarget {
    Local(LocalId),
    /// `for k, v in ...`: the element is a tuple, unpacked into these locals.
    Tuple(Vec<LocalId>),
}

#[derive(Clone, Debug)]
pub enum LoopSource {
    /// `range(start, stop, step)`, evaluated once.
    Range {
        start: Expr,
        stop: Expr,
        step: Expr,
    },
    List(Expr),
    /// One-character strings.
    Str(Expr),
    DictKeys(Expr),
    DictValues(Expr),
    /// Elements are `tuple[K, V]`.
    DictItems(Expr),
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub ty: Ty,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    /// `None`. Its type is `None` or an option type.
    None,
    /// Wrap a value as the present case of an option type.
    Some(Box<Expr>),
    Local(LocalId),
    Call {
        func: FuncId,
        args: Vec<Expr>,
    },
    Builtin {
        op: Builtin,
        args: Vec<Expr>,
    },
    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    IfElse {
        cond: Box<Expr>,
        then: Box<Expr>,
        otherwise: Box<Expr>,
    },
    StructNew {
        id: StructId,
        fields: Vec<Expr>,
    },
    Field {
        object: Box<Expr>,
        index: usize,
    },
    List(Vec<Expr>),
    Dict(Vec<(Expr, Expr)>),
    Tuple(Vec<Expr>),
    TupleGet {
        tuple: Box<Expr>,
        index: usize,
    },
    /// `list[i]` (bounds-checked) or `dict[k]` (panics if absent).
    Index {
        container: Box<Expr>,
        index: Box<Expr>,
    },
    /// `list[a:b]` or `str[a:b]`.
    Slice {
        container: Box<Expr>,
        start: Option<Box<Expr>>,
        stop: Option<Box<Expr>>,
    },
    FString(Vec<FPart>),
}

#[derive(Clone, Debug)]
pub enum FPart {
    Literal(String),
    /// A value of type `int`, `float` or `str` when there is a spec; any printable type when not.
    Value {
        expr: Expr,
        spec: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    IntNeg,
    FloatNeg,
    Not,
    IntInvert,
}

/// Operators, specialised by operand type. Comparisons work on the operand type recorded in
/// the left operand's `ty`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOp {
    IntAdd,
    IntSub,
    IntMul,
    IntFloorDiv,
    IntMod,
    IntPow,
    IntAnd,
    IntOr,
    IntXor,
    IntShl,
    IntShr,
    /// `int / int`, which gives a `float`.
    IntTrueDiv,
    FloatAdd,
    FloatSub,
    FloatMul,
    FloatDiv,
    FloatFloorDiv,
    FloatMod,
    FloatPow,
    StrConcat,
    ListConcat,
    Eq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,
    /// Identity of references; or, when the right side is `None`, whether an option is absent.
    Is,
    IsNot,
    /// Membership: substring, list element, or dict key.
    In,
    NotIn,
}

/// Built-in functions and methods of built-in types. Arguments are in order, receiver first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Builtin {
    Print,
    Panic,
    Len,
    /// `str(x)`
    ToStr,
    IntOfFloat,
    IntOfBool,
    FloatOfInt,
    AbsInt,
    AbsFloat,
    StrUpper,
    StrLower,
    StrStrip,
    StrLstrip,
    StrRstrip,
    StrSplit,
    StrSplitWhitespace,
    StrJoin,
    StrReplace,
    StrFind,
    StrStartsWith,
    StrEndsWith,
    StrContains,
    ListAppend,
    ListExtend,
    ListInsert,
    ListPopLast,
    ListPopAt,
    ListRemove,
    ListClear,
    ListSort,
    ListReverse,
    ListIndex,
    ListCopy,
    DictGet,
    DictGetOr,
    DictPop,
    DictKeys,
    DictValues,
    DictItems,
    DictClear,
    OptUnwrap,
    OptUnwrapOr,
    OptIsSome,
    OptIsNone,
}
