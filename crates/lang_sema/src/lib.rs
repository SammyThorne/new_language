//! Name resolution and type checking. Turns an AST into typed HIR, or reports why it cannot.
//!
//! Must not depend on LLVM, so that `check` stays fast and this crate can later serve a language
//! server (docs/03-compiler-architecture.md).

mod check;
pub mod hir;
pub mod stdlib;
pub mod types;

pub use check::{IndexEntry, ModuleInput, analyze, check, check_modules, check_program};
