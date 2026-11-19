//! Code generation: typed HIR to LLVM IR, then to a native object file.
//!
//! Conventions (docs/03-compiler-architecture.md, "LLVM code generation"):
//! - One LLVM module per program.
//! - User functions are internal and use `fastcc`. The program's `main` is exported as
//!   [`ENTRY_SYMBOL`] with the C calling convention; the runtime's C `main` calls it.
//! - Locals live in `alloca` slots; the `mem2reg` pass turns them into SSA values.
//! - Integer arithmetic has no `nsw`/`nuw` flags, so it wraps on overflow (docs/08, D16).
//! - Every function is `nounwind`: the language has no exceptions.
//!
//! Value representations:
//!
//! | Type | LLVM |
//! |---|---|
//! | `int`, `float`, `bool` | `i64`, `double`, `i1` |
//! | `str` | `{ ptr, i64 }`: UTF-8 bytes and their length |
//! | struct, `list`, `dict` | `ptr` to a collected object whose first word is its type descriptor |
//! | `tuple[...]` | an LLVM struct of the elements |
//! | user enum | `ptr` to an immutable object `{ ptr desc, i64 tag, fields... }`; variants without fields are static constants |
//! | trait object | `{ ptr data, ptr vtable }` |
//! | function value `def(A) -> R` | `{ ptr code, ptr env }`: code takes the environment first, then the arguments |
//! | `T?` | the same as `T` with a null first pointer for `None` when `T` is a struct, list, dict, enum, `str` or trait object; otherwise `{ i1, T }` |
//! | `Result[T, E]` | `{ i1 ok, T, E }`, leaving out `T` when it is `None` |
//!
//! A vtable is an array of pointers: the concrete type's descriptor, then the vtables of the
//! trait's required traits in declaration order, then the methods in declaration order.
//!
//! Closures: a variable captured by a lambda or nested function lives in a heap cell, and its
//! stack slot holds the cell's address. A closure's environment is an array of the cells it
//! captures. Named functions and bound methods used as values get small adapter functions that
//! take (and ignore, or unpack) an environment, so every function value is called the same way.
//!
//! Generic code is compiled only through its instances; HIR functions and types marked `generic`
//! are skipped.
//!
//! Split across files: statements (`stmt.rs`), expressions (`expr.rs`), built-in functions and
//! methods (`builtins.rs`), and per-type helper functions such as equality and printing
//! (`helpers.rs`).

mod builtins;
mod expr;
mod helpers;
mod json;
mod matching;
mod stmt;
mod traits;

use inkwell::attributes::{Attribute, AttributeLoc};
use inkwell::basic_block::BasicBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::{CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetData, TargetMachine};
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum, StructType};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValue, BasicValueEnum, FunctionValue, IntValue, PointerValue, StructValue,
};
use inkwell::{AddressSpace, OptimizationLevel};
use lang_diag::{SourceMap, Span};
use lang_sema::hir::{FuncId, Program};
use lang_sema::types::{Ty, TyKind};
use std::collections::HashMap;
use std::path::Path;

/// The symbol the runtime calls to start the program.
pub const ENTRY_SYMBOL: &str = "lang_user_main";

const CALLCONV_FAST: u32 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptLevel {
    /// Only `mem2reg`; fast to compile, easy to read in `--emit=llvm-ir`.
    Debug,
    /// LLVM's standard `default<O2>` pipeline.
    Release,
}

/// Generate the program and return its LLVM IR as text.
pub fn emit_llvm_ir(program: &Program, sources: &SourceMap, opt: OptLevel) -> Result<String, String> {
    with_module(program, sources, opt, |module, _| Ok(module.print_to_string().to_string()))
}

/// Generate the program and write a native object file for the host.
pub fn emit_object(program: &Program, sources: &SourceMap, opt: OptLevel, path: &Path) -> Result<(), String> {
    with_module(program, sources, opt, |module, machine| {
        machine
            .write_to_file(module, FileType::Object, path)
            .map_err(|e| format!("could not write `{}`: {}", path.display(), e.to_string()))
    })
}

fn with_module<R>(
    program: &Program,
    sources: &SourceMap,
    opt: OptLevel,
    then: impl FnOnce(&Module<'_>, &TargetMachine) -> Result<R, String>,
) -> Result<R, String> {
    Target::initialize_native(&InitializationConfig::default())?;
    let triple = TargetMachine::get_default_triple();
    let target = Target::from_triple(&triple).map_err(|e| e.to_string())?;
    let level = match opt {
        OptLevel::Debug => OptimizationLevel::None,
        OptLevel::Release => OptimizationLevel::Default,
    };
    let machine = target
        .create_target_machine(&triple, "generic", "", level, RelocMode::PIC, CodeModel::Default)
        .ok_or_else(|| format!("LLVM cannot generate code for `{}`", triple.as_str().to_string_lossy()))?;
    let target_data = machine.get_target_data();

    let context = Context::create();
    let module = context.create_module(&program.module);
    module.set_triple(&triple);
    module.set_data_layout(&target_data.get_data_layout());

    Codegen::new(&context, &module, &target_data, program, sources).emit();
    module.verify().map_err(|e| format!("generated LLVM IR is invalid: {}", e.to_string()))?;

    let passes = match opt {
        OptLevel::Debug => "mem2reg",
        OptLevel::Release => "default<O2>",
    };
    module
        .run_passes(passes, &machine, PassBuilderOptions::create())
        .map_err(|e| format!("LLVM optimisation failed: {}", e.to_string()))?;
    then(&module, &machine)
}

/// Builder calls only fail when the builder has no insertion point, which is a bug in this crate.
trait Must<T> {
    fn must(self) -> T;
}

impl<T, E: std::fmt::Debug> Must<T> for Result<T, E> {
    fn must(self) -> T {
        self.expect("LLVM builder call failed")
    }
}

/// Symbol names are readable and reversible: `module.function`. Only characters the assembler
/// accepts without quoting are kept in the module part.
fn symbol_name(module: &str, function: &str) -> String {
    let module: String = module.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect();
    format!("{module}.{function}")
}

/// The runtime's C-ABI functions: name, return type, parameter types. Type letters: `p` pointer,
/// `i` i64, `f` double, `s` the `{ ptr, i64 }` string pair, `v` void.
const RUNTIME: &[(&str, &str, &str)] = &[
    ("lrt_alloc", "p", "ip"),
    ("lrt_alloc_raw", "p", "i"),
    ("lrt_task_new", "p", "i"),
    ("lrt_task_result", "p", "p"),
    ("lrt_spawn", "v", "ppp"),
    ("lrt_task_join", "v", "p"),
    ("lrt_task_join_timeout", "i", "pf"),
    ("lrt_task_is_done", "i", "p"),
    ("lrt_channel_new", "p", "ii"),
    ("lrt_channel_send", "v", "pppi"),
    ("lrt_channel_recv", "i", "ppi"),
    ("lrt_channel_close", "v", "p"),
    ("lrt_mutex_new", "p", "i"),
    ("lrt_mutex_value", "p", "p"),
    ("lrt_mutex_lock", "p", "p"),
    ("lrt_mutex_unlock", "v", "p"),
    ("lrt_dict_copy", "p", "p"),
    ("lrt_dict_eq", "i", "ppp"),
    ("lrt_http_serve", "v", "ippp"),
    ("lrt_json_reader_new", "p", "pi"),
    ("lrt_json_reader_free", "v", "p"),
    ("lrt_json_finish", "i", "p"),
    ("lrt_json_failed", "i", "p"),
    ("lrt_json_error_message", "s", "p"),
    ("lrt_json_error_path", "s", "p"),
    ("lrt_json_push_key", "v", "ppi"),
    ("lrt_json_push_index", "v", "pi"),
    ("lrt_json_pop", "v", "p"),
    ("lrt_json_fail_expected", "v", "ppi"),
    ("lrt_json_fail", "v", "ppi"),
    ("lrt_json_read_null", "i", "p"),
    ("lrt_json_read_bool", "i", "p"),
    ("lrt_json_read_int", "i", "pp"),
    ("lrt_json_read_float", "i", "pp"),
    ("lrt_json_read_number", "i", "ppp"),
    ("lrt_json_read_str", "s", "p"),
    ("lrt_json_array_start", "i", "p"),
    ("lrt_json_array_next", "i", "p"),
    ("lrt_json_object_start", "i", "p"),
    ("lrt_json_object_key", "s", "p"),
    ("lrt_json_skip_value", "i", "p"),
    ("lrt_json_peek_kind", "i", "p"),
    ("lrt_json_peek_tag", "s", "ppi"),
    ("lrt_builder_push_json_str", "v", "ppi"),
    ("lrt_builder_push_json_float", "v", "pf"),
    ("lrt_builder_push_uint", "v", "pi"),
    ("lrt_io_read_line", "s", ""),
    ("llvm.fptosi.sat.i64.f64", "i", "f"),
    ("lrt_list_sort_by", "v", "pppi"),
    ("lrt_panic", "v", "pipi"),
    ("lrt_panic_index", "v", "iipi"),
    ("lrt_int_pow", "i", "iipi"),
    ("lrt_float_to_int", "i", "fpi"),
    ("lrt_builder_new", "p", ""),
    ("lrt_builder_push", "v", "ppi"),
    ("lrt_builder_push_int", "v", "pi"),
    ("lrt_builder_push_float", "v", "pf"),
    ("lrt_builder_push_str_repr", "v", "ppi"),
    ("lrt_builder_push_format_int", "v", "pipi"),
    ("lrt_builder_push_format_float", "v", "pfpi"),
    ("lrt_builder_push_format_str", "v", "ppipi"),
    ("lrt_builder_finish", "s", "p"),
    ("lrt_builder_print", "v", "p"),
    ("lrt_str_concat", "s", "pipi"),
    ("lrt_str_repeat", "s", "pii"),
    ("lrt_str_eq", "i", "pipi"),
    ("lrt_str_cmp", "i", "pipi"),
    ("lrt_str_hash", "i", "pi"),
    ("lrt_str_hash_ptr", "i", "p"),
    ("lrt_str_eq_ptr", "i", "pp"),
    ("lrt_str_find", "i", "pipi"),
    ("lrt_str_slice", "s", "piiiiipi"),
    ("lrt_str_upper", "s", "pi"),
    ("lrt_str_lower", "s", "pi"),
    ("lrt_str_strip", "s", "pi"),
    ("lrt_str_lstrip", "s", "pi"),
    ("lrt_str_rstrip", "s", "pi"),
    ("lrt_str_split", "p", "pipippi"),
    ("lrt_str_split_ws", "p", "pip"),
    ("lrt_str_join", "s", "pip"),
    ("lrt_str_replace", "s", "pipipi"),
    ("lrt_str_starts_with", "i", "pipi"),
    ("lrt_str_ends_with", "i", "pipi"),
    ("lrt_str_next_char", "s", "pip"),
    ("lrt_list_new", "p", "pi"),
    ("lrt_list_push", "p", "p"),
    ("lrt_list_insert", "p", "pi"),
    ("lrt_list_pop", "v", "pippi"),
    ("lrt_list_slice", "p", "piiii"),
    ("lrt_list_copy", "p", "p"),
    ("lrt_list_extend", "v", "pp"),
    ("lrt_list_concat", "p", "pp"),
    ("lrt_list_clear", "v", "p"),
    ("lrt_list_reverse", "v", "p"),
    ("lrt_list_index_of", "i", "ppp"),
    ("lrt_list_remove_value", "v", "ppppi"),
    ("lrt_list_sort", "v", "pp"),
    ("lrt_list_eq", "i", "ppp"),
    ("lrt_dict_new", "p", "p"),
    ("lrt_dict_find", "p", "pp"),
    ("lrt_dict_slot", "p", "pp"),
    ("lrt_dict_remove", "i", "ppp"),
    ("lrt_dict_next", "p", "pp"),
    ("lrt_dict_clear", "v", "p"),
    ("lrt_parse_int", "i", "pip"),
    ("lrt_parse_float", "i", "pip"),
    ("llvm.floor.f64", "f", "f"),
    ("llvm.fabs.f64", "f", "f"),
    ("llvm.pow.f64", "f", "ff"),
];

/// Helper functions generated per type, on first use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Helper {
    /// `(T, T) -> i1`, fastcc.
    Eq,
    /// `(ptr, ptr) -> i64`, C ABI, for the runtime: compares the values behind two pointers.
    EqPtr,
    /// `(ptr) -> i64`, C ABI, for dict keys.
    HashPtr,
    /// `(ptr, ptr) -> i64`, C ABI, three-way comparison for sorting.
    CmpPtr,
    /// `(builder, T)`, fastcc: append how the value prints. `repr` quotes strings, as inside
    /// containers.
    Push { repr: bool },
    /// `(T) -> i64`, fastcc: a derived `Hash`.
    Hash,
    /// `(T, T) -> i64`, fastcc: three-way comparison of tuples, lists and types with `Ord`.
    Cmp,
    /// `(builder, T)`, fastcc: append the value's JSON.
    JsonEncode,
    /// `(reader, ptr out) -> i1`, fastcc: decode a value from JSON into `out`.
    JsonDecode,
}

/// Per-function code generation state.
struct FnState<'ctx> {
    function: FunctionValue<'ctx>,
    /// One slot per HIR local; `None` for locals of type `None`.
    slots: Vec<Option<PointerValue<'ctx>>>,
    /// Per local: whether its slot holds the address of a heap cell with the value.
    boxed: Vec<bool>,
    local_types: Vec<Ty>,
    /// Enclosing loops: where `continue` goes, where `break` goes, and how many `with` blocks
    /// were open when the loop started.
    loops: Vec<(BasicBlock<'ctx>, BasicBlock<'ctx>, usize)>,
    /// Open `with` blocks: the local holding the value and its `exit` method. Leaving a block
    /// early runs the `exit` of each one being left, innermost first.
    cleanups: Vec<(lang_sema::hir::LocalId, lang_sema::hir::WithExit, Ty)>,
    /// The function's return type, for `?`.
    ret: Ty,
}

struct Codegen<'ctx, 'a> {
    cx: &'ctx Context,
    module: &'a Module<'ctx>,
    b: Builder<'ctx>,
    td: &'a TargetData,
    program: &'a Program,
    sources: &'a SourceMap,
    /// Indexed by [`FuncId`]; `None` for generic functions, which are not compiled.
    functions: Vec<Option<FunctionValue<'ctx>>>,
    /// Adapters that let named functions and bound methods be called as function values.
    adapters: HashMap<(FuncId, bool), FunctionValue<'ctx>>,
    /// Indexed by struct id: `{ ptr descriptor, fields... }`.
    struct_types: Vec<StructType<'ctx>>,
    /// Indexed by enum id, then variant: `{ ptr descriptor, i64 tag, fields... }`.
    variant_types: Vec<Vec<StructType<'ctx>>>,
    /// `{ ptr data, ptr vtable }`.
    dyn_type: StructType<'ctx>,
    /// The static objects of variants without fields.
    unit_variants: HashMap<(usize, usize), PointerValue<'ctx>>,
    vtables: HashMap<(Ty, lang_sema::types::TraitId), PointerValue<'ctx>>,
    /// Functions for derived `Display` and `Hash` methods, for vtables.
    derived_methods: HashMap<(lang_sema::types::TraitId, Ty), FunctionValue<'ctx>>,
    str_type: StructType<'ctx>,
    /// `{ ptr desc, i64 len, i64 cap, ptr data }`; matches `lang_rt::list::List`.
    list_type: StructType<'ctx>,
    /// `{ ptr desc, i64 len }`, the prefix of `lang_rt::dict::Dict` that generated code reads.
    dict_type: StructType<'ctx>,
    /// Matches `lang_rt::TypeDesc`.
    desc_type: StructType<'ctx>,
    strings: HashMap<String, PointerValue<'ctx>>,
    descs: HashMap<Ty, PointerValue<'ctx>>,
    helpers: HashMap<(Helper, Ty), FunctionValue<'ctx>>,
    nounwind: Attribute,
    state: Option<FnState<'ctx>>,
}

impl<'ctx, 'a> Codegen<'ctx, 'a> {
    fn new(
        cx: &'ctx Context,
        module: &'a Module<'ctx>,
        td: &'a TargetData,
        program: &'a Program,
        sources: &'a SourceMap,
    ) -> Self {
        let ptr = cx.ptr_type(AddressSpace::default());
        let i64_type = cx.i64_type();
        let struct_types =
            program.structs.iter().map(|s| cx.opaque_struct_type(&format!("struct.{}", s.name))).collect();
        let variant_types = program
            .enums
            .iter()
            .map(|e| e.variants.iter().map(|v| cx.opaque_struct_type(&format!("enum.{}.{}", e.name, v.name))).collect())
            .collect();
        let codegen = Codegen {
            cx,
            module,
            b: cx.create_builder(),
            td,
            program,
            sources,
            functions: Vec::new(),
            adapters: HashMap::new(),
            struct_types,
            variant_types,
            dyn_type: cx.struct_type(&[ptr.into(), ptr.into()], false),
            unit_variants: HashMap::new(),
            vtables: HashMap::new(),
            derived_methods: HashMap::new(),
            str_type: cx.struct_type(&[ptr.into(), i64_type.into()], false),
            list_type: cx.struct_type(&[ptr.into(), i64_type.into(), i64_type.into(), ptr.into()], false),
            dict_type: cx.struct_type(&[ptr.into(), i64_type.into()], false),
            desc_type: cx.struct_type(
                &[
                    i64_type.into(),
                    i64_type.into(),
                    ptr.into(),
                    i64_type.into(),
                    i64_type.into(),
                    i64_type.into(),
                    i64_type.into(),
                    i64_type.into(),
                    ptr.into(),
                    ptr.into(),
                ],
                false,
            ),
            strings: HashMap::new(),
            descs: HashMap::new(),
            helpers: HashMap::new(),
            nounwind: cx.create_enum_attribute(Attribute::get_named_enum_kind_id("nounwind"), 0),
            state: None,
        };
        // Struct bodies can refer to each other, so they are filled in after all are declared.
        for (index, def) in program.structs.iter().enumerate() {
            if def.generic {
                continue;
            }
            let mut fields: Vec<BasicTypeEnum> = vec![ptr.into()];
            fields.extend(def.fields.iter().map(|f| codegen.llty(f.ty).expect("fields always have a value type")));
            codegen.struct_types[index].set_body(&fields, false);
        }
        for (enum_index, def) in program.enums.iter().enumerate() {
            if def.generic {
                continue;
            }
            for (variant_index, variant) in def.variants.iter().enumerate() {
                let mut fields: Vec<BasicTypeEnum> = vec![ptr.into(), i64_type.into()];
                fields.extend(
                    variant.fields.iter().map(|f| codegen.llty(f.ty).expect("fields always have a value type")),
                );
                codegen.variant_types[enum_index][variant_index].set_body(&fields, false);
            }
        }
        codegen
    }

    /// The structure of a type. The immutable collections are represented exactly like their
    /// mutable counterparts, so they are reported as those.
    fn kind(&self, ty: Ty) -> &'a TyKind {
        let types = &self.program.types;
        match types.kind(ty) {
            TyKind::FrozenList(elem) => {
                types.kind(types.lookup(&TyKind::List(*elem)).expect("created with the frozen type"))
            }
            TyKind::FrozenDict(k, v) => {
                types.kind(types.lookup(&TyKind::Dict(*k, *v)).expect("created with the frozen type"))
            }
            kind => kind,
        }
    }

    // ---- types --------------------------------------------------------------------------

    fn ptr_type(&self) -> inkwell::types::PointerType<'ctx> {
        self.cx.ptr_type(AddressSpace::default())
    }

    /// Whether values of this type are a single pointer to a collected object.
    fn is_reference(&self, ty: Ty) -> bool {
        matches!(
            self.kind(ty),
            TyKind::Struct(_)
                | TyKind::List(_)
                | TyKind::Dict(..)
                | TyKind::Enum(_)
                | TyKind::Task(_)
                | TyKind::Channel(_)
                | TyKind::Mutex(_)
                | TyKind::Guard(_)
                | TyKind::Atomic
        )
    }

    /// Values that are a pair whose first part is a non-null pointer: strings and trait objects.
    fn is_pointer_pair(&self, ty: Ty) -> bool {
        matches!(self.kind(ty), TyKind::Str | TyKind::Dyn(_) | TyKind::SelfType(_) | TyKind::Func(..))
    }

    /// Whether `T?` represents `None` as a null pointer, with no separate flag.
    fn option_is_nullable(&self, inner: Ty) -> bool {
        self.is_reference(inner) || self.is_pointer_pair(inner)
    }

    /// The LLVM type of a value, or `None` for the `None` type, which has no runtime value.
    fn llty(&self, ty: Ty) -> Option<BasicTypeEnum<'ctx>> {
        Some(match self.kind(ty) {
            TyKind::None => return None,
            TyKind::Error => unreachable!("HIR never contains the error type"),
            TyKind::Int => self.cx.i64_type().into(),
            TyKind::Float => self.cx.f64_type().into(),
            TyKind::Sized(kind) if kind.is_float() => self.cx.f32_type().into(),
            TyKind::Sized(kind) => self.int_type(kind.bits()).into(),
            TyKind::Bool => self.cx.bool_type().into(),
            TyKind::Str => self.str_type.into(),
            TyKind::Struct(_) | TyKind::List(_) | TyKind::Dict(..) | TyKind::Enum(_) => self.ptr_type().into(),
            TyKind::Task(_) | TyKind::Channel(_) | TyKind::Mutex(_) | TyKind::Guard(_) | TyKind::Atomic => {
                self.ptr_type().into()
            }
            TyKind::FrozenList(_) | TyKind::FrozenDict(..) => unreachable!("normalised by `kind`"),
            TyKind::Dyn(_) | TyKind::SelfType(_) | TyKind::Func(..) => self.dyn_type.into(),
            TyKind::Param(_) => unreachable!("generic code is not compiled"),
            TyKind::Result(ok, err) => {
                let mut fields: Vec<BasicTypeEnum> = vec![self.cx.bool_type().into()];
                fields.extend(self.llty(*ok));
                fields.push(self.llty(*err).expect("errors have a value type"));
                self.cx.struct_type(&fields, false).into()
            }
            TyKind::Tuple(items) => {
                let fields: Vec<BasicTypeEnum> =
                    items.iter().map(|t| self.llty(*t).expect("tuple elements have values")).collect();
                self.cx.struct_type(&fields, false).into()
            }
            TyKind::Option(inner) => {
                let inner_type = self.llty(*inner).expect("optional types wrap a value type");
                if self.option_is_nullable(*inner) {
                    inner_type
                } else {
                    self.cx.struct_type(&[self.cx.bool_type().into(), inner_type], false).into()
                }
            }
        })
    }

    /// The LLVM integer type with this many bits.
    fn int_type(&self, bits: u32) -> inkwell::types::IntType<'ctx> {
        match bits {
            8 => self.cx.i8_type(),
            16 => self.cx.i16_type(),
            32 => self.cx.i32_type(),
            _ => self.cx.i64_type(),
        }
    }

    fn value_type(&self, ty: Ty) -> BasicTypeEnum<'ctx> {
        self.llty(ty).expect("a value type")
    }

    /// Bytes one value occupies in memory, including padding.
    fn size_of(&self, ty: Ty) -> u64 {
        self.td.get_abi_size(&self.value_type(ty))
    }

    /// Whether a value of this type contains any reference the collector must see.
    fn has_pointers(&self, ty: Ty) -> bool {
        match self.kind(ty) {
            TyKind::Str | TyKind::Struct(_) | TyKind::List(_) | TyKind::Dict(..) => true,
            TyKind::Enum(_) | TyKind::Dyn(_) | TyKind::SelfType(_) | TyKind::Func(..) => true,
            TyKind::Task(_) | TyKind::Channel(_) | TyKind::Mutex(_) | TyKind::Guard(_) | TyKind::Atomic => true,
            TyKind::Option(inner) => self.has_pointers(*inner),
            TyKind::Result(ok, err) => self.has_pointers(*ok) || self.has_pointers(*err),
            TyKind::Tuple(items) => items.iter().any(|t| self.has_pointers(*t)),
            _ => false,
        }
    }

    /// Byte offset of a dict entry's value: after the 8-byte tag and the key padded to 8.
    fn dict_value_offset(&self, key: Ty) -> u64 {
        8 + self.size_of(key).div_ceil(8) * 8
    }

    // ---- constants ----------------------------------------------------------------------

    fn i64(&self, value: i64) -> IntValue<'ctx> {
        self.cx.i64_type().const_int(value as u64, true)
    }

    fn bool(&self, value: bool) -> IntValue<'ctx> {
        self.cx.bool_type().const_int(u64::from(value), false)
    }

    /// Pointer to the bytes of a string constant, emitted once per distinct text.
    fn bytes_const(&mut self, text: &str) -> PointerValue<'ctx> {
        if let Some(&ptr) = self.strings.get(text) {
            return ptr;
        }
        let bytes = self.cx.const_string(text.as_bytes(), false);
        let global = self.module.add_global(bytes.get_type(), None, &format!(".str.{}", self.strings.len()));
        global.set_initializer(&bytes);
        global.set_constant(true);
        global.set_linkage(Linkage::Private);
        global.set_unnamed_addr(true);
        global.set_alignment(1);
        let ptr = global.as_pointer_value();
        self.strings.insert(text.to_string(), ptr);
        ptr
    }

    /// A `str` constant.
    fn str_const(&mut self, text: &str) -> StructValue<'ctx> {
        let ptr = self.bytes_const(text);
        let len = self.i64(text.len() as i64);
        self.str_type.const_named_struct(&[ptr.into(), len.into()])
    }

    /// A source location, `file:line:col`, for panic messages.
    fn location(&mut self, span: Span) -> (PointerValue<'ctx>, IntValue<'ctx>) {
        let file = self.sources.get(span.file);
        let (line, col) = file.line_col(span.lo as usize);
        let text = format!("{}:{line}:{col}", file.name);
        (self.bytes_const(&text), self.i64(text.len() as i64))
    }

    /// The type descriptor of a struct, list or dict type (layout: `lang_rt::TypeDesc`).
    fn desc(&mut self, ty: Ty) -> PointerValue<'ctx> {
        if let Some(&desc) = self.descs.get(&ty) {
            return desc;
        }
        let null = self.ptr_type().const_null();
        let name = self.program.types.display(ty);
        let name_ptr = self.bytes_const(&name);
        let (size, bitmap, elem_size, elem_ptrs, value_size, value_ptrs, hash, eq) = match self.kind(ty).clone() {
            TyKind::Struct(id) => {
                let struct_type = self.struct_types[id.0];
                let mut bitmap = 0u64;
                for (index, field) in self.program.structs[id.0].fields.iter().enumerate() {
                    let offset = self.td.offset_of_element(&struct_type, index as u32 + 1).unwrap_or(0);
                    if self.has_pointers(field.ty) && offset / 8 < 64 {
                        bitmap |= 1 << (offset / 8);
                    }
                }
                (self.td.get_abi_size(&struct_type), bitmap, 0, 0, 0, 0, null, null)
            }
            TyKind::List(elem) => (
                self.td.get_abi_size(&self.list_type),
                0b1001,
                self.size_of(elem),
                self.has_pointers(elem) as u64,
                0,
                0,
                null,
                null,
            ),
            TyKind::Dict(key, value) => {
                // `str` keys use the runtime's functions directly; other keys use generated ones.
                let (hash, eq) = if key == Ty::STR {
                    (
                        self.runtime("lrt_str_hash_ptr").as_global_value().as_pointer_value(),
                        self.runtime("lrt_str_eq_ptr").as_global_value().as_pointer_value(),
                    )
                } else {
                    (
                        self.helper(Helper::HashPtr, key).as_global_value().as_pointer_value(),
                        self.helper(Helper::EqPtr, key).as_global_value().as_pointer_value(),
                    )
                };
                (
                    0,
                    0,
                    self.size_of(key),
                    self.has_pointers(key) as u64,
                    self.size_of(value),
                    self.has_pointers(value) as u64,
                    hash,
                    eq,
                )
            }
            TyKind::Enum(_) => (0, 0, 0, 0, 0, 0, null, null),
            other => unreachable!("no descriptor for {other:?}"),
        };
        let int = |v: u64| self.cx.i64_type().const_int(v, false).as_basic_value_enum();
        let init = self.desc_type.const_named_struct(&[
            int(size),
            int(bitmap),
            name_ptr.into(),
            int(name.len() as u64),
            int(elem_size),
            int(elem_ptrs),
            int(value_size),
            int(value_ptrs),
            hash.into(),
            eq.into(),
        ]);
        let global = self.module.add_global(self.desc_type, None, &format!("desc.{name}"));
        global.set_initializer(&init);
        global.set_constant(true);
        global.set_linkage(Linkage::Private);
        let ptr = global.as_pointer_value();
        self.descs.insert(ty, ptr);
        ptr
    }

    // ---- runtime calls ------------------------------------------------------------------

    fn letter_type(&self, letter: char) -> BasicTypeEnum<'ctx> {
        match letter {
            'p' => self.ptr_type().into(),
            'i' => self.cx.i64_type().into(),
            'f' => self.cx.f64_type().into(),
            's' => self.str_type.into(),
            other => unreachable!("unknown type letter {other}"),
        }
    }

    fn runtime(&self, name: &str) -> FunctionValue<'ctx> {
        if let Some(function) = self.module.get_function(name) {
            return function;
        }
        let &(_, ret, params) =
            RUNTIME.iter().find(|(n, _, _)| *n == name).unwrap_or_else(|| panic!("unknown runtime function {name}"));
        let params: Vec<BasicMetadataTypeEnum> = params.chars().map(|c| self.letter_type(c).into()).collect();
        let fn_type = match ret {
            "v" => self.cx.void_type().fn_type(&params, false),
            r => self.letter_type(r.chars().next().unwrap()).fn_type(&params, false),
        };
        let function = self.module.add_function(name, fn_type, Some(Linkage::External));
        function.add_attribute(AttributeLoc::Function, self.nounwind);
        if name.starts_with("lrt_panic") {
            for attr in ["noreturn", "cold"] {
                let attribute = self.cx.create_enum_attribute(Attribute::get_named_enum_kind_id(attr), 0);
                function.add_attribute(AttributeLoc::Function, attribute);
            }
        }
        function
    }

    fn call_rt(&mut self, name: &str, args: &[BasicValueEnum<'ctx>]) -> Option<BasicValueEnum<'ctx>> {
        let function = self.runtime(name);
        let args: Vec<BasicMetadataValueEnum> = args.iter().map(|a| (*a).into()).collect();
        self.b.build_call(function, &args, "").must().try_as_basic_value().basic()
    }

    /// The pointer and length of a `str` value, as runtime arguments.
    fn str_parts(&self, value: BasicValueEnum<'ctx>) -> [BasicValueEnum<'ctx>; 2] {
        let value = value.into_struct_value();
        [self.b.build_extract_value(value, 0, "").must(), self.b.build_extract_value(value, 1, "").must()]
    }

    // ---- functions and blocks -----------------------------------------------------------

    fn current_function(&self) -> FunctionValue<'ctx> {
        self.state.as_ref().expect("inside a function").function
    }

    fn block(&self, name: &str) -> BasicBlock<'ctx> {
        self.cx.append_basic_block(self.current_function(), name)
    }

    /// Continue emitting in a fresh block that nothing jumps to, after a terminator.
    fn start_dead_block(&mut self) {
        let dead = self.block("dead");
        self.b.position_at_end(dead);
    }

    /// An `alloca` in the function's entry block, where `mem2reg` can promote it.
    fn alloca(&mut self, ty: BasicTypeEnum<'ctx>, name: &str) -> PointerValue<'ctx> {
        let entry = self.current_function().get_first_basic_block().expect("functions have an entry block");
        let builder = self.cx.create_builder();
        match entry.get_first_instruction() {
            Some(first) => builder.position_before(&first),
            None => builder.position_at_end(entry),
        }
        builder.build_alloca(ty, name).must()
    }

    /// Store a value in a fresh stack slot and return its address, for runtime calls that take
    /// values by pointer.
    fn spill(&mut self, value: BasicValueEnum<'ctx>) -> PointerValue<'ctx> {
        let slot = self.alloca(value.get_type(), "spill");
        self.b.build_store(slot, value).must();
        slot
    }

    /// Run `body` with the builder inside a new function, then return to where it was.
    fn in_function<R>(&mut self, function: FunctionValue<'ctx>, body: impl FnOnce(&mut Self) -> R) -> R {
        let saved_block = self.b.get_insert_block();
        let saved_state = self.state.take();
        let entry = self.cx.append_basic_block(function, "entry");
        self.b.position_at_end(entry);
        self.state = Some(FnState {
            function,
            slots: Vec::new(),
            boxed: Vec::new(),
            local_types: Vec::new(),
            loops: Vec::new(),
            cleanups: Vec::new(),
            ret: Ty::NONE,
        });
        let result = body(self);
        self.state = saved_state;
        if let Some(block) = saved_block {
            self.b.position_at_end(block);
        }
        result
    }

    /// Branch to a cold block that calls `fail` (which must not return) when `failed` is true.
    fn check(&mut self, failed: IntValue<'ctx>, fail: impl FnOnce(&mut Self)) {
        let fail_block = self.block("panic");
        let ok_block = self.block("ok");
        self.b.build_conditional_branch(failed, fail_block, ok_block).must();
        self.b.position_at_end(fail_block);
        fail(self);
        self.b.build_unreachable().must();
        self.b.position_at_end(ok_block);
    }

    fn panic_message(&mut self, message: &str, span: Span) {
        let message = self.str_const(message);
        let (loc, loc_len) = self.location(span);
        let [ptr, len] = self.str_parts(message.into());
        self.call_rt("lrt_panic", &[ptr, len, loc.into(), loc_len.into()]);
    }

    // ---- emission -----------------------------------------------------------------------

    fn emit(mut self) {
        for (index, function) in self.program.functions.iter().enumerate() {
            if function.generic {
                self.functions.push(None);
                continue;
            }
            let mut params: Vec<BasicMetadataTypeEnum> = Vec::new();
            if function.closure {
                params.push(self.ptr_type().into());
            }
            params.extend(
                function
                    .params
                    .iter()
                    .map(|local| -> BasicMetadataTypeEnum { self.value_type(function.locals[local.0].ty).into() }),
            );
            let fn_type = match self.llty(function.ret) {
                Some(ret) => ret.fn_type(&params, false),
                None => self.cx.void_type().fn_type(&params, false),
            };
            let value = if FuncId(index) == self.program.entry {
                self.module.add_function(ENTRY_SYMBOL, fn_type, Some(Linkage::External))
            } else {
                let value = self.module.add_function(
                    &symbol_name(&self.program.module, &function.name),
                    fn_type,
                    Some(Linkage::Internal),
                );
                value.set_call_conventions(CALLCONV_FAST);
                value
            };
            value.add_attribute(AttributeLoc::Function, self.nounwind);
            let skip = usize::from(function.closure);
            for (param, local) in value.get_param_iter().skip(skip).zip(&function.params) {
                param.set_name(&function.locals[local.0].name);
            }
            self.functions.push(Some(value));
        }
        for index in 0..self.program.functions.len() {
            if !self.program.functions[index].generic {
                self.define(FuncId(index));
            }
        }
    }

    /// The LLVM function for a HIR function, which must not be generic.
    fn function_value(&self, id: FuncId) -> FunctionValue<'ctx> {
        self.functions[id.0].expect("generic functions are never called from compiled code")
    }

    fn define(&mut self, id: FuncId) {
        let function = &self.program.functions[id.0];
        let value = self.function_value(id);
        if let Some(symbol) = &function.extern_symbol {
            self.define_extern(value, function, symbol);
            return;
        }
        self.in_function(value, |this| {
            let ptr = this.ptr_type();
            let slots: Vec<Option<PointerValue>> = function
                .locals
                .iter()
                .map(|local| {
                    let ty = this.llty(local.ty)?;
                    let slot_type = if local.boxed { ptr.into() } else { ty };
                    Some(this.b.build_alloca(slot_type, &local.name).must())
                })
                .collect();
            // Captured variables: the cell comes from the environment. Other boxed variables
            // get a new cell.
            let env = function.closure.then(|| value.get_nth_param(0).unwrap().into_pointer_value());
            for (index, local) in function.locals.iter().enumerate() {
                let Some(slot) = slots[index] else { continue };
                if let Some(capture) = local.capture {
                    let env = env.expect("closures have an environment");
                    // SAFETY (LLVM): the environment holds one pointer per capture.
                    let at = unsafe { this.b.build_in_bounds_gep(ptr, env, &[this.i64(capture as i64)], "").must() };
                    let cell = this.b.build_load(ptr, at, "cell").must();
                    this.b.build_store(slot, cell).must();
                } else if local.boxed {
                    let size = this.i64(this.size_of(local.ty) as i64);
                    let cell = this.call_rt("lrt_alloc_raw", &[size.into()]).unwrap();
                    this.b.build_store(slot, cell).must();
                }
            }
            let skip = usize::from(function.closure);
            for (param, local) in value.get_param_iter().skip(skip).zip(&function.params) {
                let slot = slots[local.0].expect("parameters have a slot");
                if function.locals[local.0].boxed {
                    let cell = this.b.build_load(ptr, slot, "").must().into_pointer_value();
                    this.b.build_store(cell, param).must();
                } else {
                    this.b.build_store(slot, param).must();
                }
            }
            let state = this.state.as_mut().unwrap();
            state.slots = slots;
            state.boxed = function.locals.iter().map(|l| l.boxed).collect();
            state.local_types = function.locals.iter().map(|l| l.ty).collect();
            state.ret = function.ret;
            this.stmts(&function.body);
            if function.ret == Ty::NONE {
                this.b.build_return(None).must();
            } else {
                // The type checker guarantees a value-returning function never reaches its end.
                this.b.build_unreachable().must();
            }
        });
    }
}

impl<'ctx> Codegen<'ctx, '_> {
    /// The body of an `@extern` function: call the C function, passing each `str` as a pointer
    /// and a length, and return its result.
    fn define_extern(&mut self, value: FunctionValue<'ctx>, function: &lang_sema::hir::Function, symbol: &str) {
        let ptr = self.ptr_type();
        let mut c_params: Vec<BasicMetadataTypeEnum> = Vec::new();
        for local in &function.params {
            let ty = function.locals[local.0].ty;
            if ty == Ty::STR {
                c_params.push(ptr.into());
                c_params.push(self.cx.i64_type().into());
            } else {
                c_params.push(self.value_type(ty).into());
            }
        }
        let c_type = match self.llty(function.ret) {
            Some(ret) => ret.fn_type(&c_params, false),
            None => self.cx.void_type().fn_type(&c_params, false),
        };
        let c_function = self
            .module
            .get_function(symbol)
            .unwrap_or_else(|| self.module.add_function(symbol, c_type, Some(Linkage::External)));
        self.in_function(value, |this| {
            let mut args: Vec<BasicMetadataValueEnum> = Vec::new();
            for (param, local) in value.get_param_iter().zip(&function.params) {
                if function.locals[local.0].ty == Ty::STR {
                    let [p, len] = this.str_parts(param);
                    args.push(p.into());
                    args.push(len.into());
                } else {
                    args.push(param.into());
                }
            }
            let call = this.b.build_call(c_function, &args, "").must();
            match call.try_as_basic_value().basic() {
                Some(result) => this.b.build_return(Some(&result)).must(),
                None => this.b.build_return(None).must(),
            };
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lang_diag::{Diagnostics, FileId};

    fn compile(text: &str, opt: OptLevel) -> String {
        let mut sources = SourceMap::default();
        let file = sources.add("test.src", text);
        let mut diags = Diagnostics::default();
        let tokens = lang_syntax::lex(file, text, &mut diags);
        let module = lang_syntax::parse(&tokens, &mut diags);
        let program = lang_sema::check(&module, &mut sources, "test", &mut diags).expect("test program checks");
        assert_eq!(file, FileId(0));
        emit_llvm_ir(&program, &sources, opt).unwrap()
    }

    #[test]
    fn hello_world_ir() {
        let ir = compile("def main():\n    print(\"Hello\")\n", OptLevel::Debug);
        assert!(ir.contains(&format!("define void @{ENTRY_SYMBOL}()")), "{ir}");
        assert!(ir.contains("c\"Hello\""), "{ir}");
        assert!(ir.contains("@lrt_builder_print"), "{ir}");
    }

    #[test]
    fn release_build_verifies() {
        let ir = compile("def main():\n    print(\"Hello\")\n", OptLevel::Release);
        assert!(ir.contains(ENTRY_SYMBOL));
    }

    #[test]
    fn symbol_names_are_readable() {
        assert_eq!(symbol_name("my-app", "greet"), "my_app.greet");
    }
}
