//! What the language server offers after a `.` and at the start of a name (docs/11, milestone 5):
//! the members of every type the program mentions, the public names of every module, and the
//! names at the top level of the file.

use super::{Checker, Global, builtins};
use std::collections::HashMap;

/// What kind of thing a completion is, which editors show as an icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemberKind {
    Field,
    Method,
    Function,
    Type,
    Module,
    Constant,
}

/// One name on offer, with a short description (a type or a signature).
#[derive(Clone, Debug)]
pub struct Member {
    pub name: String,
    pub kind: MemberKind,
    pub detail: String,
}

/// Everything completion draws on.
#[derive(Clone, Debug, Default)]
pub struct Completions {
    /// Fields and methods, by the type's name as it is shown (`User`, `Box[int]`).
    pub by_type: HashMap<String, Vec<Member>>,
    /// Methods of the built-in types, by the start of the type's name: `str`, `list`, `dict`, `set`...
    pub builtin: HashMap<&'static str, Vec<Member>>,
    /// The public names of each module, by the module's name.
    pub modules: HashMap<String, Vec<Member>>,
    /// The names at the top of the file being checked, and the language's own.
    pub scope: Vec<Member>,
}

impl Checker<'_, '_> {
    pub(super) fn completions(&self) -> Completions {
        let mut out = Completions::default();
        for info in &self.structs {
            let shown = self.types.display(info.ty);
            let mut members: Vec<Member> = info
                .fields
                .iter()
                .filter(|f| !f.private)
                .map(|f| Member { name: f.name.clone(), kind: MemberKind::Field, detail: self.types.display(f.ty) })
                .collect();
            members.extend(self.methods_of(info.ty));
            out.by_type.insert(shown, members);
        }
        for info in &self.enums {
            out.by_type.insert(self.types.display(info.ty), self.methods_of(info.ty));
        }
        for (prefix, names) in builtins::builtin_method_names() {
            let mut members: Vec<Member> = names
                .iter()
                .map(|name| Member { name: (*name).to_string(), kind: MemberKind::Method, detail: String::new() })
                .collect();
            let start = format!("__{prefix}_");
            for (name, (global, _)) in &self.globals {
                if let (Some(method), Some(detail)) = (name.strip_prefix(&start), self.function_detail(*global))
                    && !members.iter().any(|m| m.name == method)
                {
                    members.push(Member { name: method.to_string(), kind: MemberKind::Method, detail });
                }
            }
            members.sort_by(|a, b| a.name.cmp(&b.name));
            out.builtin.insert(prefix, members);
        }
        for (index, exports) in self.exports.iter().enumerate().skip(1) {
            let mut members: Vec<Member> = exports
                .iter()
                .filter(|(name, (_, _, public))| *public && !name.starts_with("__"))
                .filter_map(|(name, (global, _, _))| self.global_member(name, *global))
                .collect();
            members.sort_by(|a, b| a.name.cmp(&b.name));
            out.modules.insert(self.module_names[index].clone(), members);
        }
        let entry = self.module_names.len().saturating_sub(1);
        let scope = if entry == self.current_module { &self.globals } else { &self.module_scopes[entry] };
        let mut names: Vec<Member> = scope
            .iter()
            .filter(|(name, _)| !name.starts_with("__"))
            .filter_map(|(name, (global, _))| self.global_member(name, *global))
            .collect();
        // The language's own functions and types, which are not in any scope's table.
        for name in super::BUILTIN_FUNCTIONS.iter().filter(|n| !n.starts_with("__")) {
            if !names.iter().any(|m| m.name == *name) {
                names.push(Member { name: (*name).to_string(), kind: MemberKind::Function, detail: "built-in".into() });
            }
        }
        for name in super::KNOWN_TYPES.iter().chain(super::BUILTIN_TYPES) {
            if !names.iter().any(|m| m.name == *name) {
                names.push(Member {
                    name: (*name).to_string(),
                    kind: MemberKind::Type,
                    detail: "built-in type".into(),
                });
            }
        }
        for name in super::BUILTIN_CONSTRUCTORS {
            names.push(Member { name: (*name).to_string(), kind: MemberKind::Function, detail: "built-in".into() });
        }
        names.sort_by(|a, b| a.name.cmp(&b.name));
        out.scope = names;
        out
    }

    /// The methods of a type, from its `impl` blocks and the traits it implements.
    fn methods_of(&self, ty: crate::types::Ty) -> Vec<Member> {
        let mut members = Vec::new();
        let mut add = |name: &str, detail: String| {
            if !name.starts_with("__") && !members.iter().any(|m: &Member| m.name == name) {
                members.push(Member { name: name.to_string(), kind: MemberKind::Method, detail });
            }
        };
        for ((owner, name), func) in &self.inherent {
            if *owner == ty && self.signatures[func.0].receiver.is_some() {
                add(name, self.signature_text(*func));
            }
        }
        for ((owner, name), index) in &self.generic_methods {
            if *owner == ty
                && let Some(pattern) = self.generic_fns[*index].pattern
                && self.signatures[pattern.0].receiver.is_some()
            {
                add(name, self.signature_text(pattern));
            }
        }
        for ((owner, name), traits) in &self.trait_methods {
            if *owner == ty {
                let names: Vec<&str> = traits.iter().map(|(t, _)| self.traits[t.0].name.as_str()).collect();
                add(name, format!("from {}", names.join(", ")));
            }
        }
        members.sort_by(|a, b| a.name.cmp(&b.name));
        members
    }

    fn signature_text(&self, func: crate::hir::FuncId) -> String {
        let signature = &self.signatures[func.0];
        let params: Vec<String> =
            signature.params.iter().map(|p| format!("{}: {}", p.name, self.types.display(p.ty))).collect();
        match signature.ret {
            crate::types::Ty::NONE => format!("({})", params.join(", ")),
            ret => format!("({}) -> {}", params.join(", "), self.types.display(ret)),
        }
    }

    fn function_detail(&self, global: Global) -> Option<String> {
        match global {
            Global::Function(func) => Some(self.signature_text(func)),
            Global::GenericFunction(index) => {
                self.generic_fns[index].pattern.map(|pattern| self.signature_text(pattern))
            }
            _ => None,
        }
    }

    fn global_member(&self, name: &str, global: Global) -> Option<Member> {
        let (kind, detail) = match global {
            Global::Function(_) | Global::GenericFunction(_) => {
                (MemberKind::Function, self.function_detail(global).unwrap_or_default())
            }
            Global::Struct(id) => (MemberKind::Type, format!("struct {}", self.structs[id.0].name)),
            Global::Enum(id) => (MemberKind::Type, format!("enum {}", self.enums[id.0].name)),
            Global::GenericStruct(_) => (MemberKind::Type, "generic struct".to_string()),
            Global::GenericEnum(_) => (MemberKind::Type, "generic enum".to_string()),
            Global::Trait(id) => (MemberKind::Type, format!("trait {}", self.traits[id.0].name)),
            Global::TypeAlias(_) => (MemberKind::Type, "type alias".to_string()),
            Global::Const(index) => (
                MemberKind::Constant,
                self.consts[index].value.as_ref().map(|v| self.types.display(v.ty)).unwrap_or_default(),
            ),
            Global::Module(_) => (MemberKind::Module, "module".to_string()),
            Global::Missing => return None,
        };
        Some(Member { name: name.to_string(), kind, detail })
    }
}
