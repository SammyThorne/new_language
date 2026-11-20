//! Types, interned: each distinct type is stored once and referred to by a small copyable
//! [`Ty`], so comparing types is comparing integers (docs/02-type-system-and-traits.md).
//!
//! Questions that depend on trait implementations (can this type be printed, hashed, compared?)
//! are answered by the type checker, which knows the implementations; this module only knows
//! the structure of types.

use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Ty(u32);

impl Ty {
    /// The type of an expression that already produced an error. It is compatible with
    /// everything, so one mistake does not cause a cascade of follow-on errors.
    pub const ERROR: Ty = Ty(0);
    /// The type of "no value": the result of a function with no `->`.
    pub const NONE: Ty = Ty(1);
    pub const BOOL: Ty = Ty(2);
    /// 64-bit signed integer; arithmetic wraps on overflow (docs/08, D16).
    pub const INT: Ty = Ty(3);
    pub const FLOAT: Ty = Ty(4);
    pub const STR: Ty = Ty(5);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StructId(pub usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EnumId(pub usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TraitId(pub usize);

/// The sized numeric types, for binding C functions and compact data (docs/01, "Built-in types").
/// `int` is 64-bit signed and `float` 64-bit; `i64` and `f64` are other names for them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SizedKind {
    I8,
    I16,
    I32,
    U8,
    U16,
    U32,
    U64,
    F32,
}

impl SizedKind {
    pub const ALL: [SizedKind; 8] = [
        SizedKind::I8,
        SizedKind::I16,
        SizedKind::I32,
        SizedKind::U8,
        SizedKind::U16,
        SizedKind::U32,
        SizedKind::U64,
        SizedKind::F32,
    ];

    pub fn name(self) -> &'static str {
        match self {
            SizedKind::I8 => "i8",
            SizedKind::I16 => "i16",
            SizedKind::I32 => "i32",
            SizedKind::U8 => "u8",
            SizedKind::U16 => "u16",
            SizedKind::U32 => "u32",
            SizedKind::U64 => "u64",
            SizedKind::F32 => "f32",
        }
    }

    pub fn from_name(name: &str) -> Option<SizedKind> {
        SizedKind::ALL.into_iter().find(|k| k.name() == name)
    }

    pub fn bits(self) -> u32 {
        match self {
            SizedKind::I8 | SizedKind::U8 => 8,
            SizedKind::I16 | SizedKind::U16 => 16,
            SizedKind::I32 | SizedKind::U32 | SizedKind::F32 => 32,
            SizedKind::U64 => 64,
        }
    }

    pub fn is_float(self) -> bool {
        self == SizedKind::F32
    }

    pub fn is_signed(self) -> bool {
        matches!(self, SizedKind::I8 | SizedKind::I16 | SizedKind::I32 | SizedKind::F32)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TyKind {
    Error,
    None,
    Bool,
    Int,
    Float,
    Str,
    List(Ty),
    Dict(Ty, Ty),
    Tuple(Vec<Ty>),
    /// `T?`: a `T` or `None`. The built-in generic enum with `Some(T)` and `None`.
    Option(Ty),
    /// `Result[T, E]`: the built-in generic enum with `Ok(T)` and `Err(E)`.
    Result(Ty, Ty),
    Struct(StructId),
    Enum(EnumId),
    /// A trait used as a type: a trait object, dispatched dynamically.
    Dyn(TraitId),
    /// `Self` while checking a trait's default methods, before they are copied into each
    /// implementation. Only the trait's own methods can be used on it.
    SelfType(TraitId),
    /// A type parameter, while checking a generic declaration against its bounds. The number
    /// indexes the type checker's table of parameters.
    Param(u32),
    /// A function value: `def(int, str) -> bool`.
    Func(Vec<Ty>, Ty),
    /// `i8` to `u64` and `f32`.
    Sized(SizedKind),
    /// `tuple[T, ...]`: an immutable list, which can be shared between tasks.
    FrozenList(Ty),
    /// `frozendict[K, V]`: an immutable dict.
    FrozenDict(Ty, Ty),
    /// `Task[T]`: a running `spawn`, whose `join()` gives a `T`.
    Task(Ty),
    /// `Channel[T]`: a queue between tasks.
    Channel(Ty),
    /// `Mutex[T]`: a value reachable only while holding its lock.
    Mutex(Ty),
    /// What `mutex.lock()` returns: usable only as the value of a `with` block.
    Guard(Ty),
    /// `Atomic`: an `int` updated without a lock.
    Atomic,
}

#[derive(Debug)]
pub struct Types {
    kinds: Vec<TyKind>,
    ids: HashMap<TyKind, Ty>,
    struct_names: Vec<String>,
    enum_names: Vec<String>,
    trait_names: Vec<String>,
    param_names: Vec<String>,
    /// The built-in `Error` trait, so that `Result[T, Error]` can be shown as `Result[T]`.
    pub error_trait: Option<TraitId>,
}

impl Default for Types {
    fn default() -> Self {
        Types::new()
    }
}

impl Types {
    pub fn new() -> Types {
        let mut types = Types {
            kinds: Vec::new(),
            ids: HashMap::new(),
            struct_names: Vec::new(),
            enum_names: Vec::new(),
            trait_names: Vec::new(),
            param_names: Vec::new(),
            error_trait: None,
        };
        for (kind, expected) in [
            (TyKind::Error, Ty::ERROR),
            (TyKind::None, Ty::NONE),
            (TyKind::Bool, Ty::BOOL),
            (TyKind::Int, Ty::INT),
            (TyKind::Float, Ty::FLOAT),
            (TyKind::Str, Ty::STR),
        ] {
            assert_eq!(types.intern(kind), expected);
        }
        types
    }

    pub fn intern(&mut self, kind: TyKind) -> Ty {
        if let Some(&ty) = self.ids.get(&kind) {
            return ty;
        }
        let ty = Ty(self.kinds.len() as u32);
        self.kinds.push(kind.clone());
        self.ids.insert(kind, ty);
        ty
    }

    /// The type with this structure, if it has been created.
    pub fn lookup(&self, kind: &TyKind) -> Option<Ty> {
        self.ids.get(kind).copied()
    }

    pub fn kind(&self, ty: Ty) -> &TyKind {
        &self.kinds[ty.0 as usize]
    }

    pub fn list(&mut self, elem: Ty) -> Ty {
        self.intern(TyKind::List(elem))
    }

    pub fn dict(&mut self, key: Ty, value: Ty) -> Ty {
        self.intern(TyKind::Dict(key, value))
    }

    /// `set[T]`, which is a dict whose values are `None` and take no room (docs/11, milestone 3).
    /// Everything that works on a dict's keys, such as `in`, `len`, iteration and equality, works
    /// on it unchanged; the checker allows a set only what a set has.
    pub fn set(&mut self, elem: Ty) -> Ty {
        self.intern(TyKind::Dict(elem, Ty::NONE))
    }

    /// The element type of a `list[T]`.
    pub fn list_elem(&self, ty: Ty) -> Option<Ty> {
        match self.kind(ty) {
            TyKind::List(elem) => Some(*elem),
            _ => None,
        }
    }

    /// The element type of a `set[T]`.
    pub fn set_elem(&self, ty: Ty) -> Option<Ty> {
        match self.kind(ty) {
            TyKind::Dict(elem, value) if *value == Ty::NONE => Some(*elem),
            _ => None,
        }
    }

    pub fn tuple(&mut self, items: Vec<Ty>) -> Ty {
        self.intern(TyKind::Tuple(items))
    }

    pub fn option(&mut self, inner: Ty) -> Ty {
        self.intern(TyKind::Option(inner))
    }

    pub fn result(&mut self, ok: Ty, err: Ty) -> Ty {
        self.intern(TyKind::Result(ok, err))
    }

    pub fn dyn_(&mut self, id: TraitId) -> Ty {
        self.intern(TyKind::Dyn(id))
    }

    pub fn self_type(&mut self, id: TraitId) -> Ty {
        self.intern(TyKind::SelfType(id))
    }

    pub fn add_struct(&mut self, name: &str) -> (StructId, Ty) {
        let id = StructId(self.struct_names.len());
        self.struct_names.push(name.to_string());
        (id, self.intern(TyKind::Struct(id)))
    }

    pub fn add_enum(&mut self, name: &str) -> (EnumId, Ty) {
        let id = EnumId(self.enum_names.len());
        self.enum_names.push(name.to_string());
        (id, self.intern(TyKind::Enum(id)))
    }

    /// A new type parameter; `index` is its position in the type checker's table.
    pub fn add_param(&mut self, name: &str) -> (u32, Ty) {
        let index = self.param_names.len() as u32;
        self.param_names.push(name.to_string());
        (index, self.intern(TyKind::Param(index)))
    }

    pub fn func(&mut self, params: Vec<Ty>, ret: Ty) -> Ty {
        self.intern(TyKind::Func(params, ret))
    }

    /// `tuple[T, ...]`. The mutable `list[T]` is created too: code generation represents both
    /// the same way.
    pub fn frozen_list(&mut self, elem: Ty) -> Ty {
        self.list(elem);
        self.intern(TyKind::FrozenList(elem))
    }

    /// `frozendict[K, V]`, creating `dict[K, V]` too.
    pub fn frozen_dict(&mut self, key: Ty, value: Ty) -> Ty {
        self.dict(key, value);
        self.intern(TyKind::FrozenDict(key, value))
    }

    pub fn sized(&mut self, kind: SizedKind) -> Ty {
        self.intern(TyKind::Sized(kind))
    }

    /// The sized numeric kind of a type, if it is one.
    pub fn sized_kind(&self, ty: Ty) -> Option<SizedKind> {
        match self.kind(ty) {
            TyKind::Sized(kind) => Some(*kind),
            _ => None,
        }
    }

    pub fn add_trait(&mut self, name: &str) -> TraitId {
        let id = TraitId(self.trait_names.len());
        self.trait_names.push(name.to_string());
        id
    }

    pub fn struct_name(&self, id: StructId) -> &str {
        &self.struct_names[id.0]
    }

    pub fn enum_name(&self, id: EnumId) -> &str {
        &self.enum_names[id.0]
    }

    pub fn trait_name(&self, id: TraitId) -> &str {
        &self.trait_names[id.0]
    }

    /// How the type is written in source: `list[int]`, `str?`, `Result[int]`.
    pub fn display(&self, ty: Ty) -> String {
        match self.kind(ty) {
            TyKind::Error => "{error}".into(),
            TyKind::None => "None".into(),
            TyKind::Bool => "bool".into(),
            TyKind::Int => "int".into(),
            TyKind::Float => "float".into(),
            TyKind::Str => "str".into(),
            TyKind::Sized(kind) => kind.name().into(),
            TyKind::FrozenList(elem) => format!("tuple[{}, ...]", self.display(*elem)),
            TyKind::FrozenDict(key, value) => format!("frozendict[{}, {}]", self.display(*key), self.display(*value)),
            TyKind::Task(t) => format!("Task[{}]", self.display(*t)),
            TyKind::Channel(t) => format!("Channel[{}]", self.display(*t)),
            TyKind::Mutex(t) => format!("Mutex[{}]", self.display(*t)),
            TyKind::Guard(t) => format!("MutexGuard[{}]", self.display(*t)),
            TyKind::Atomic => "Atomic".into(),
            TyKind::List(elem) => format!("list[{}]", self.display(*elem)),
            TyKind::Dict(elem, value) if *value == Ty::NONE => format!("set[{}]", self.display(*elem)),
            TyKind::Dict(key, value) => format!("dict[{}, {}]", self.display(*key), self.display(*value)),
            TyKind::Tuple(items) => {
                format!("tuple[{}]", items.iter().map(|t| self.display(*t)).collect::<Vec<_>>().join(", "))
            }
            TyKind::Option(inner) => format!("{}?", self.display(*inner)),
            TyKind::Result(ok, err) => {
                let default_error = self.error_trait.is_some_and(|e| self.kind(*err) == &TyKind::Dyn(e));
                if default_error {
                    format!("Result[{}]", self.display(*ok))
                } else {
                    format!("Result[{}, {}]", self.display(*ok), self.display(*err))
                }
            }
            TyKind::Struct(id) => self.struct_name(*id).to_string(),
            TyKind::Enum(id) => self.enum_name(*id).to_string(),
            TyKind::Dyn(id) => self.trait_name(*id).to_string(),
            TyKind::SelfType(_) => "Self".to_string(),
            TyKind::Param(index) => self.param_names[*index as usize].clone(),
            TyKind::Func(params, ret) => {
                let params = params.iter().map(|t| self.display(*t)).collect::<Vec<_>>().join(", ");
                if *ret == Ty::NONE {
                    format!("def({params})")
                } else {
                    format!("def({params}) -> {}", self.display(*ret))
                }
            }
        }
    }

    /// Whether the type mentions a type parameter anywhere.
    pub fn contains_param(&self, ty: Ty) -> bool {
        match self.kind(ty) {
            TyKind::Param(_) => true,
            TyKind::List(t)
            | TyKind::Option(t)
            | TyKind::FrozenList(t)
            | TyKind::Task(t)
            | TyKind::Channel(t)
            | TyKind::Mutex(t)
            | TyKind::Guard(t) => self.contains_param(*t),
            TyKind::Dict(k, v) | TyKind::Result(k, v) | TyKind::FrozenDict(k, v) => {
                self.contains_param(*k) || self.contains_param(*v)
            }
            TyKind::Tuple(items) => items.iter().any(|t| self.contains_param(*t)),
            TyKind::Func(params, ret) => params.iter().any(|t| self.contains_param(*t)) || self.contains_param(*ret),
            _ => false,
        }
    }

    pub fn contains_error(&self, ty: Ty) -> bool {
        match self.kind(ty) {
            TyKind::Error => true,
            TyKind::List(t)
            | TyKind::Option(t)
            | TyKind::FrozenList(t)
            | TyKind::Task(t)
            | TyKind::Channel(t)
            | TyKind::Mutex(t)
            | TyKind::Guard(t) => self.contains_error(*t),
            TyKind::Dict(k, v) | TyKind::Result(k, v) | TyKind::FrozenDict(k, v) => {
                self.contains_error(*k) || self.contains_error(*v)
            }
            TyKind::Tuple(items) => items.iter().any(|t| self.contains_error(*t)),
            TyKind::Func(params, ret) => params.iter().any(|t| self.contains_error(*t)) || self.contains_error(*ret),
            _ => false,
        }
    }

    /// Whether a value of type `actual` can be used where `expected` is required, without any
    /// conversion. Types containing errors fit anything, to avoid follow-on errors.
    pub fn fits(&self, actual: Ty, expected: Ty) -> bool {
        actual == expected || self.contains_error(actual) || self.contains_error(expected)
    }

    /// Values that are a single pointer to a mutable heap object, and so can be compared with
    /// `is`. Enums are heap objects too, but immutable values, so `is` does not apply.
    pub fn is_reference(&self, ty: Ty) -> bool {
        matches!(
            self.kind(ty),
            TyKind::Struct(_)
                | TyKind::List(_)
                | TyKind::Dict(..)
                | TyKind::Channel(_)
                | TyKind::Mutex(_)
                | TyKind::Task(_)
        )
    }

    pub fn option_inner(&self, ty: Ty) -> Option<Ty> {
        match self.kind(ty) {
            TyKind::Option(inner) => Some(*inner),
            _ => None,
        }
    }

    pub fn result_parts(&self, ty: Ty) -> Option<(Ty, Ty)> {
        match self.kind(ty) {
            TyKind::Result(ok, err) => Some((*ok, *err)),
            _ => None,
        }
    }
}
