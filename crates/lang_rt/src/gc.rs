//! Memory allocation through the Boehm–Demers–Weiser conservative collector.
//!
//! The compiler needs nothing from the collector except calls to these functions: Boehm finds
//! roots by scanning stacks, registers and static data (docs/04-runtime-and-memory.md).
//!
//! The collector is the system's `libgc.so.1`. Its declarations are written out here so that
//! building needs no development headers.
//!
//! Environment variables (names from `lang_names::env_var`):
//! - `GC_STRESS`: collect before every allocation, to make missing roots crash immediately.
//! - `GC_INITIAL_HEAP`: bytes to reserve at startup.
//! - `GC_FREE_SPACE_DIVISOR`: Boehm's heap growth control; lower values grow the heap faster and
//!   collect less often.

use crate::TypeDesc;
use std::sync::atomic::{AtomicBool, Ordering};

#[link(name = "libgc.so.1", kind = "dylib", modifiers = "+verbatim")]
unsafe extern "C" {
    fn GC_init();
    fn GC_malloc(size: usize) -> *mut u8;
    fn GC_malloc_atomic(size: usize) -> *mut u8;
    fn GC_gcollect();
    fn GC_expand_hp(bytes: usize) -> i32;
    fn GC_set_free_space_divisor(divisor: usize);
}

static STRESS: AtomicBool = AtomicBool::new(false);

fn env_number(suffix: &str) -> Option<usize> {
    std::env::var(lang_names::env_var(suffix)).ok().and_then(|v| v.trim().parse().ok())
}

/// Initialise the collector. Called once, from the main thread, before any allocation.
pub fn init() {
    // SAFETY: called once at startup on the main thread, as Boehm requires.
    unsafe { GC_init() };
    let stress = std::env::var(lang_names::env_var("GC_STRESS")).is_ok_and(|v| !v.is_empty() && v != "0");
    STRESS.store(stress, Ordering::Relaxed);
    if let Some(divisor) = env_number("GC_FREE_SPACE_DIVISOR").filter(|&d| d > 0) {
        // SAFETY: plain configuration call.
        unsafe { GC_set_free_space_divisor(divisor) };
    }
    if let Some(bytes) = env_number("GC_INITIAL_HEAP") {
        // SAFETY: plain configuration call; failure only means the heap grows later.
        unsafe { GC_expand_hp(bytes) };
    }
}

fn out_of_memory(size: usize) -> ! {
    crate::out::flush();
    eprintln!("fatal: out of memory allocating {size} bytes");
    std::process::abort()
}

/// Allocate zeroed memory that the collector scans for references.
pub fn alloc(size: usize) -> *mut u8 {
    if STRESS.load(Ordering::Relaxed) {
        // SAFETY: collection may run at any allocation.
        unsafe { GC_gcollect() };
    }
    // SAFETY: plain allocation.
    let ptr = unsafe { GC_malloc(size.max(1)) };
    if ptr.is_null() {
        out_of_memory(size)
    }
    ptr
}

/// Allocate memory the collector never scans, for bytes that cannot contain references (string
/// contents, buffers of numbers). Not zeroed.
pub fn alloc_atomic(size: usize) -> *mut u8 {
    if STRESS.load(Ordering::Relaxed) {
        // SAFETY: collection may run at any allocation.
        unsafe { GC_gcollect() };
    }
    // SAFETY: plain allocation.
    let ptr = unsafe { GC_malloc_atomic(size.max(1)) };
    if ptr.is_null() {
        out_of_memory(size)
    }
    ptr
}

/// Allocate an object whose first word is its type descriptor. Used for structs.
///
/// # Safety
/// `size` must be at least one word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lrt_alloc(size: i64, desc: *const TypeDesc) -> *mut u8 {
    let ptr = alloc(size as usize);
    // SAFETY: the allocation is at least one word and suitably aligned.
    unsafe { *(ptr as *mut *const TypeDesc) = desc };
    ptr
}
