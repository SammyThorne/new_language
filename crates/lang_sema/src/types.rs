//! Types, interned: each distinct type is stored once and referred to by a small copyable
//! [`Ty`], so comparing types is comparing integers (docs/02-type-system-and-traits.md).

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
    /// `T?`: a `T` or `None`.
    Option(Ty),
    Struct(StructId),
}

#[derive(Debug)]
pub struct Types {
    kinds: Vec<TyKind>,
    ids: HashMap<TyKind, Ty>,
    struct_names: Vec<String>,
}

impl Default for Types {
    fn default() -> Self {
        Types::new()
    }
}

impl Types {
    pub fn new() -> Types {
        let mut types = Types { kinds: Vec::new(), ids: HashMap::new(), struct_names: Vec::new() };
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

    pub fn kind(&self, ty: Ty) -> &TyKind {
        &self.kinds[ty.0 as usize]
    }

    pub fn list(&mut self, elem: Ty) -> Ty {
        self.intern(TyKind::List(elem))
    }

    pub fn dict(&mut self, key: Ty, value: Ty) -> Ty {
        self.intern(TyKind::Dict(key, value))
    }

    pub fn tuple(&mut self, items: Vec<Ty>) -> Ty {
        self.intern(TyKind::Tuple(items))
    }

    pub fn option(&mut self, inner: Ty) -> Ty {
        self.intern(TyKind::Option(inner))
    }

    pub fn add_struct(&mut self, name: &str) -> (StructId, Ty) {
        let id = StructId(self.struct_names.len());
        self.struct_names.push(name.to_string());
        (id, self.intern(TyKind::Struct(id)))
    }

    pub fn struct_name(&self, id: StructId) -> &str {
        &self.struct_names[id.0]
    }

    /// How the type is written in source: `list[int]`, `str?`, `tuple[int, str]`.
    pub fn display(&self, ty: Ty) -> String {
        match self.kind(ty) {
            TyKind::Error => "{error}".into(),
            TyKind::None => "None".into(),
            TyKind::Bool => "bool".into(),
            TyKind::Int => "int".into(),
            TyKind::Float => "float".into(),
            TyKind::Str => "str".into(),
            TyKind::List(elem) => format!("list[{}]", self.display(*elem)),
            TyKind::Dict(key, value) => format!("dict[{}, {}]", self.display(*key), self.display(*value)),
            TyKind::Tuple(items) => {
                format!("tuple[{}]", items.iter().map(|t| self.display(*t)).collect::<Vec<_>>().join(", "))
            }
            TyKind::Option(inner) => format!("{}?", self.display(*inner)),
            TyKind::Struct(id) => self.struct_name(*id).to_string(),
        }
    }

    pub fn contains_error(&self, ty: Ty) -> bool {
        match self.kind(ty) {
            TyKind::Error => true,
            TyKind::List(t) | TyKind::Option(t) => self.contains_error(*t),
            TyKind::Dict(k, v) => self.contains_error(*k) || self.contains_error(*v),
            TyKind::Tuple(items) => items.iter().any(|t| self.contains_error(*t)),
            _ => false,
        }
    }

    /// Whether a value of type `actual` can be used where `expected` is required, without any
    /// conversion. Types containing errors fit anything, to avoid follow-on errors.
    pub fn fits(&self, actual: Ty, expected: Ty) -> bool {
        actual == expected || self.contains_error(actual) || self.contains_error(expected)
    }

    /// Usable as a dict key (docs/02: the `Hash` trait, built in for these types).
    pub fn is_hashable(&self, ty: Ty) -> bool {
        match self.kind(ty) {
            TyKind::Error | TyKind::Int | TyKind::Bool | TyKind::Str => true,
            TyKind::Tuple(items) => items.iter().all(|t| self.is_hashable(*t)),
            _ => false,
        }
    }

    /// Supports `==` (the built-in `Eq` implementations). Structs get `==` with traits.
    pub fn is_equatable(&self, ty: Ty) -> bool {
        match self.kind(ty) {
            TyKind::Error | TyKind::None | TyKind::Bool | TyKind::Int | TyKind::Float | TyKind::Str => true,
            TyKind::List(t) | TyKind::Option(t) => self.is_equatable(*t),
            TyKind::Tuple(items) => items.iter().all(|t| self.is_equatable(*t)),
            TyKind::Dict(..) | TyKind::Struct(_) => false,
        }
    }

    /// Supports `<` and sorting.
    pub fn is_orderable(&self, ty: Ty) -> bool {
        matches!(self.kind(ty), TyKind::Error | TyKind::Bool | TyKind::Int | TyKind::Float | TyKind::Str)
    }

    /// Can be printed or converted with `str()`. Structs need the `Display` trait (milestone 3).
    pub fn is_displayable(&self, ty: Ty) -> bool {
        match self.kind(ty) {
            TyKind::Struct(_) => false,
            TyKind::List(t) | TyKind::Option(t) => self.is_displayable(*t),
            TyKind::Dict(k, v) => self.is_displayable(*k) && self.is_displayable(*v),
            TyKind::Tuple(items) => items.iter().all(|t| self.is_displayable(*t)),
            _ => true,
        }
    }

    /// Values that are a single pointer to a heap object, and so can be compared with `is`.
    pub fn is_reference(&self, ty: Ty) -> bool {
        matches!(self.kind(ty), TyKind::Struct(_) | TyKind::List(_) | TyKind::Dict(..))
    }

    pub fn option_inner(&self, ty: Ty) -> Option<Ty> {
        match self.kind(ty) {
            TyKind::Option(inner) => Some(*inner),
            _ => None,
        }
    }
}
