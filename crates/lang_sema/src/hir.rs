//! HIR: the typed program handed to code generation.
//!
//! Every name is resolved, every expression has a type, operators are specialised to their
//! operand types, and sugar (augmented assignment, tuple unpacking, `elif`, field defaults) is
//! gone. HIR only exists for programs without errors, so it never contains [`Ty::ERROR`].

use crate::types::{StructId, TraitId, Ty, Types};
use lang_diag::Span;
use std::collections::HashMap;

#[derive(Debug)]
pub struct Program {
    /// Module name, used to build symbol names. Taken from the file name.
    pub module: String,
    pub types: Types,
    /// Indexed by [`StructId`].
    pub structs: Vec<StructDef>,
    /// Indexed by [`crate::types::EnumId`].
    pub enums: Vec<EnumDef>,
    /// Indexed by [`TraitId`].
    pub traits: Vec<TraitDef>,
    /// Every trait implementation, explicit or derived.
    pub impls: Vec<ImplDef>,
    pub impl_index: HashMap<(TraitId, Ty), usize>,
    /// Functions and methods, indexed by [`FuncId`].
    pub functions: Vec<Function>,
    /// The program's `main`.
    pub entry: FuncId,
    pub builtins: BuiltinTraits,
    /// The prelude's `ParseError`, the error type of `int.parse` and `float.parse`.
    pub parse_error: StructId,
}

impl Program {
    pub fn impl_of(&self, trait_id: TraitId, ty: Ty) -> Option<&ImplDef> {
        self.impl_index.get(&(trait_id, ty)).map(|&i| &self.impls[i])
    }
}

/// The traits the compiler gives meaning to, all defined in the prelude.
#[derive(Clone, Copy, Debug)]
pub struct BuiltinTraits {
    pub display: TraitId,
    pub eq: TraitId,
    pub hash: TraitId,
    pub copy: TraitId,
    pub error: TraitId,
    pub add: TraitId,
    pub sub: TraitId,
    pub mul: TraitId,
    pub ord: TraitId,
    pub number: TraitId,
}

#[derive(Debug)]
pub struct EnumDef {
    pub name: String,
    pub ty: Ty,
    pub variants: Vec<VariantDef>,
    /// As for [`StructDef::generic`].
    pub generic: bool,
}

#[derive(Debug)]
pub struct VariantDef {
    pub name: String,
    pub fields: Vec<FieldDef>,
}

#[derive(Debug)]
pub struct TraitDef {
    pub name: String,
    /// Direct required traits, in declaration order. A vtable holds the vtables of these.
    pub supertraits: Vec<TraitId>,
    pub methods: Vec<TraitMethodDef>,
}

#[derive(Debug)]
pub struct TraitMethodDef {
    pub name: String,
    /// Parameter types after `self`. Only meaningful for object-safe traits, whose methods never
    /// mention `Self` except as the receiver.
    pub params: Vec<Ty>,
    pub ret: Ty,
}

#[derive(Debug)]
pub struct ImplDef {
    pub trait_id: TraitId,
    pub ty: Ty,
    /// The function implementing each trait method, in the trait's order. `None` when the
    /// implementation is derived, and code generation produces it.
    pub methods: Vec<Option<FuncId>>,
    pub derived: bool,
}

#[derive(Debug)]
pub struct StructDef {
    pub name: String,
    pub ty: Ty,
    pub fields: Vec<FieldDef>,
    /// An instance of a generic struct whose type arguments are still parameters, used only
    /// to check generic code. It has no layout and generates no code.
    pub generic: bool,
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
    /// Checked against its bounds but not compiled: a generic function before instantiation,
    /// or code using such types. Code generation skips it.
    pub generic: bool,
    /// A lambda or nested function. It takes a hidden first argument, its environment: the
    /// cells of the variables it captures.
    pub closure: bool,
    /// Bound to a C function with `@extern("symbol")`; it has no body. Code generation emits a
    /// wrapper that converts the arguments to the C calling convention.
    pub extern_symbol: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Local {
    pub name: String,
    pub ty: Ty,
    /// Kept in a heap cell rather than on the stack, because a closure captures it.
    pub boxed: bool,
    /// In a closure: the variable is the enclosing function's, found in this slot of the
    /// environment.
    pub capture: Option<usize>,
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
    /// Cases are tried in order; the type checker guarantees one matches.
    Match {
        subject: Expr,
        cases: Vec<MatchCase>,
    },
    Break,
    Continue,
    /// `with`: `object` holds the value; its `exit` method runs however the body is left,
    /// including by `return`, `break`, `continue` and `?`.
    With {
        object: LocalId,
        exit: FuncId,
        body: Block,
    },
}

#[derive(Clone, Debug)]
pub struct MatchCase {
    pub pattern: Pattern,
    pub guard: Option<Expr>,
    pub body: Block,
}

#[derive(Clone, Debug)]
pub struct Pattern {
    pub kind: PatternKind,
    pub ty: Ty,
}

#[derive(Clone, Debug)]
pub enum PatternKind {
    Wild,
    Bind(LocalId),
    /// Equal to a literal of type `int`, `float`, `str` or `bool`.
    Literal(Expr),
    /// A variant of a user enum, by index, with patterns for its fields.
    Variant {
        index: usize,
        fields: Vec<Pattern>,
    },
    Some(Box<Pattern>),
    None,
    Ok(Box<Pattern>),
    Err(Box<Pattern>),
    Tuple(Vec<Pattern>),
    Or(Vec<Pattern>),
}

/// How a value is converted to a related type: used for implicit conversions to trait objects
/// and for the error of `?`.
#[derive(Clone, Debug)]
pub enum Conversion {
    Identity,
    /// A struct or enum to a trait object.
    ToDyn(TraitId),
    /// A trait object to one of its required traits, following vtable slots.
    Upcast(Vec<usize>),
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
    /// A user-defined iterator: `next` is called until it returns `None`.
    Iterator {
        iterator: Expr,
        next: FuncId,
    },
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
    /// A user enum value: the variant's index and its field values.
    Variant {
        index: usize,
        fields: Vec<Expr>,
    },
    Ok(Box<Expr>),
    Err(Box<Expr>),
    /// A struct or enum as a trait object (type `Dyn(trait_id)`).
    ToDyn {
        value: Box<Expr>,
        trait_id: TraitId,
    },
    /// A trait object as an object of one of its required traits.
    Upcast {
        value: Box<Expr>,
        path: Vec<usize>,
    },
    /// A method call through a trait object's vtable. `path` leads from the receiver's trait to
    /// `trait_id`, which declares method number `method`.
    DynCall {
        receiver: Box<Expr>,
        trait_id: TraitId,
        path: Vec<usize>,
        method: usize,
        args: Vec<Expr>,
    },
    /// `value?`: the `Ok` or `Some` value, or an early return. `conversion` turns the error into
    /// the function's error type.
    Try {
        value: Box<Expr>,
        conversion: Conversion,
    },
    /// A lambda or nested function value, capturing these locals' cells.
    Closure {
        func: FuncId,
        captures: Vec<LocalId>,
    },
    /// A named function used as a value.
    FuncRef(FuncId),
    /// `object.method` without a call: a function value that calls the method on the object.
    BoundMethod {
        receiver: Box<Expr>,
        func: FuncId,
    },
    /// A call to a function value.
    CallValue {
        callee: Box<Expr>,
        args: Vec<Expr>,
    },
    /// Statements, then a value: what a comprehension becomes.
    Block {
        stmts: Vec<Stmt>,
        value: Box<Expr>,
    },
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
    ResultUnwrap,
    ResultUnwrapOr,
    ResultUnwrapErr,
    ResultIsOk,
    ResultIsErr,
    /// `int.parse(text)`, giving `Result[int, ParseError]`.
    ParseInt,
    ParseFloat,
    /// `x.copy()` for a type with a derived `Copy`.
    Copy,
    /// `x.hash()` for a type with a derived `Hash`.
    Hash,
    /// `a.cmp(b)`: negative, zero or positive.
    Cmp,
    /// `min(a, b, ...)` and `max(a, b, ...)` over two or more values.
    Min,
    Max,
    /// `min(list)` and `max(list)`; panic on an empty list.
    MinList,
    MaxList,
    Sum,
    /// `enumerate(list)`, `zip(a, b)`, `any(list)`, `all(list)`.
    Enumerate,
    Zip,
    Any,
    All,
    /// `list.sort(key=f, reverse=r)`: args are the list, the key function or `None`, and the
    /// `reverse` flag.
    ListSortBy,
    /// Convert between `int`, `float` and the sized number types; the target is the result
    /// type. Integers wrap; floats convert to integers by truncating toward zero.
    NumConvert,
    /// `input()`: a line from standard input, or `None` at the end.
    Input,
}
