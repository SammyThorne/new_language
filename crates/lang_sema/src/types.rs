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
            TyKind::List(elem) => format!("list[{}]", self.display(*elem)),
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
            TyKind::List(t) | TyKind::Option(t) => self.contains_param(*t),
            TyKind::Dict(k, v) | TyKind::Result(k, v) => self.contains_param(*k) || self.contains_param(*v),
            TyKind::Tuple(items) => items.iter().any(|t| self.contains_param(*t)),
            TyKind::Func(params, ret) => params.iter().any(|t| self.contains_param(*t)) || self.contains_param(*ret),
            _ => false,
        }
    }

    pub fn contains_error(&self, ty: Ty) -> bool {
        match self.kind(ty) {
            TyKind::Error => true,
            TyKind::List(t) | TyKind::Option(t) => self.contains_error(*t),
            TyKind::Dict(k, v) | TyKind::Result(k, v) => self.contains_error(*k) || self.contains_error(*v),
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
        matches!(self.kind(ty), TyKind::Struct(_) | TyKind::List(_) | TyKind::Dict(..))
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
