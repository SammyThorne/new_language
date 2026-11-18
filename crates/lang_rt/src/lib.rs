//! The runtime library, linked into every compiled program as `liblang_rt.a`.
//!
//! Generated code calls the C-ABI functions in this crate, all prefixed `lrt_`. The ABI is
//! internal: it changes together with the compiler (docs/04-runtime-and-memory.md).
//!
//! Rules for every exported function:
//! - Only integers, floats, pointers and `repr(C)` values cross the boundary. Strings cross as a
//!   pointer and a byte length, never zero-terminated, and are always valid UTF-8.
//! - A Rust panic must never unwind into generated code; `extern "C"` aborts if one tries.
//! - **The Rust heap rule:** a reference to a garbage-collected object is never stored in memory
//!   from Rust's allocator, because the collector does not scan it. Such references live on the
//!   stack during a call or inside other collected objects.

pub mod builder;
pub mod dict;
pub mod gc;
pub mod list;
pub mod num;
pub mod out;
pub mod panic;
pub mod strings;
pub mod sys;

/// Program entry. The C runtime calls this; it runs the compiled program's `main`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn main(argc: std::ffi::c_int, argv: *const *const std::ffi::c_char) -> std::ffi::c_int {
    unsafe extern "C" {
        /// The compiled program's `main`, emitted by the code generator.
        fn lang_user_main();
    }
    gc::init();
    sys::set_args(argc as i64, argv);
    // SAFETY: the code generator always defines `lang_user_main` as `void(void)` with the C
    // calling convention.
    unsafe { lang_user_main() };
    out::flush();
    0
}

/// Describes a heap object's type. Emitted as a constant by the code generator for every struct,
/// list and dict type; the layout here must match `desc_type` in `lang_codegen`.
#[repr(C)]
pub struct TypeDesc {
    /// Size of the object in bytes (structs).
    pub size: i64,
    /// Which words of the object hold references. Unused by the conservative collector; kept for
    /// a precise collector later.
    pub pointer_bitmap: i64,
    pub name: *const u8,
    pub name_len: i64,
    /// Lists: element size. Dicts: key size.
    pub elem_size: i64,
    pub elem_has_pointers: i64,
    /// Dicts: value size.
    pub value_size: i64,
    pub value_has_pointers: i64,
    /// Dicts: hash of a key, given a pointer to it.
    pub hash: Option<HashFn>,
    /// Dicts: equality of two keys, given pointers to them. Returns 0 or 1.
    pub eq: Option<EqFn>,
}

pub type HashFn = unsafe extern "C" fn(*const u8) -> i64;
pub type EqFn = unsafe extern "C" fn(*const u8, *const u8) -> i64;
/// Three-way comparison for sorting: negative, zero or positive.
pub type CmpFn = unsafe extern "C" fn(*const u8, *const u8) -> i64;

/// A string value: a pointer to UTF-8 bytes and their length. Returned by value, which the C ABI
/// on x86-64 puts in two registers.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Str {
    pub ptr: *const u8,
    pub len: i64,
}

static EMPTY: u8 = 0;

impl Str {
    /// The empty string. Its pointer is never null, because a null pointer means `None` for a
    /// `str?`.
    pub fn empty() -> Str {
        Str { ptr: &EMPTY, len: 0 }
    }

    /// Copy bytes into a new collected string.
    pub fn from_bytes(bytes: &[u8]) -> Str {
        if bytes.is_empty() {
            return Str::empty();
        }
        let ptr = gc::alloc_atomic(bytes.len());
        // SAFETY: `ptr` is a fresh allocation of `bytes.len()` bytes.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len()) };
        Str { ptr, len: bytes.len() as i64 }
    }

    /// # Safety
    /// `ptr` must point to `len` bytes of valid UTF-8 that stay alive for `'a`.
    pub unsafe fn from_raw<'a>(ptr: *const u8, len: i64) -> &'a str {
        if len <= 0 {
            return "";
        }
        // SAFETY: guaranteed by the caller; strings produced by the compiler and runtime are
        // always UTF-8.
        unsafe { std::str::from_utf8_unchecked(std::slice::from_raw_parts(ptr, len as usize)) }
    }
}

/// A source location string such as `app.wb:3:5`, passed by generated code to functions that can
/// panic.
///
/// # Safety
/// `ptr` and `len` must describe a UTF-8 string.
pub(crate) unsafe fn location<'a>(ptr: *const u8, len: i64) -> &'a str {
    unsafe { Str::from_raw(ptr, len) }
}
