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
//!   collect less often. The default is 2: a collection after allocating about half the live
//!   data. 1 (about the live data, like Go's `GOGC=100`) was the default until the minimum below
//!   took over its job for small heaps; for large ones it cost up to 40% more memory for a few
//!   percent of time (docs/08, D73 and D86). Boehm's own default is 3.
//! - `GC_MIN_ALLOC`: the least the program allocates between collections, in bytes; 4 MiB by
//!   default, Go's minimum heap. Without it, a program whose live data is small collects after
//!   every megabyte or so, which for a busy server is a thousand times a second (docs/08, D84).
//! - `ALLOC_STATS`: at exit, print the bytes allocated, the number of collections and the heap
//!   size to standard error (`bench/http/counts.py` divides them by the requests served).

use std::sync::atomic::{AtomicBool, Ordering};

#[link(name = "libgc.so.1", kind = "dylib", modifiers = "+verbatim")]
unsafe extern "C" {
    fn GC_init();
    fn GC_malloc(size: usize) -> *mut u8;
    fn GC_malloc_atomic(size: usize) -> *mut u8;
    fn GC_malloc_many(size: usize) -> *mut u8;
    fn GC_malloc_uncollectable(size: usize) -> *mut u8;
    fn GC_gcollect();
    fn GC_expand_hp(bytes: usize) -> i32;
    fn GC_set_free_space_divisor(divisor: usize);
    fn GC_set_min_bytes_allocd(bytes: usize);
    fn GC_get_total_bytes() -> usize;
    fn GC_get_gc_no() -> usize;
    fn GC_get_heap_size() -> usize;
}

unsafe extern "C" {
    fn atexit(callback: extern "C" fn()) -> i32;
}

extern "C" fn print_alloc_stats() {
    // SAFETY: plain queries.
    let (bytes, collections, heap) = unsafe { (GC_get_total_bytes(), GC_get_gc_no(), GC_get_heap_size()) };
    eprintln!("alloc stats: bytes={bytes} collections={collections} heap={heap}");
}

static STRESS: AtomicBool = AtomicBool::new(false);

/// The least a program allocates between collections, whatever the size of its live data.
const MIN_ALLOC: usize = 4 << 20;

fn env_number(suffix: &str) -> Option<usize> {
    std::env::var(lang_names::env_var(suffix)).ok().and_then(|v| v.trim().parse().ok())
}

/// Initialise the collector. Called once, from the main thread, before any allocation.
pub fn init() {
    // SAFETY: called once at startup on the main thread, as Boehm requires.
    unsafe { GC_init() };
    let stress = std::env::var(lang_names::env_var("GC_STRESS")).is_ok_and(|v| !v.is_empty() && v != "0");
    STRESS.store(stress, Ordering::Relaxed);
    let divisor = env_number("GC_FREE_SPACE_DIVISOR").filter(|&d| d > 0).unwrap_or(2);
    // SAFETY: plain configuration call.
    unsafe { GC_set_free_space_divisor(divisor) };
    let min_alloc = env_number("GC_MIN_ALLOC").filter(|&b| b > 0).unwrap_or(MIN_ALLOC);
    // SAFETY: plain configuration call.
    unsafe { GC_set_min_bytes_allocd(min_alloc) };
    if let Some(bytes) = env_number("GC_INITIAL_HEAP") {
        // SAFETY: plain configuration call; failure only means the heap grows later.
        unsafe { GC_expand_hp(bytes) };
    }
    if std::env::var(lang_names::env_var("ALLOC_STATS")).is_ok_and(|v| !v.is_empty() && v != "0") {
        // SAFETY: registering a function with no preconditions.
        unsafe { atexit(print_alloc_stats) };
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

/// Allocate zeroed, scanned memory: structs and enum variants (whose descriptor the generated
/// code then stores in the first word), the cells of captured variables, and the environments of
/// closures. The code generator declares this as an allocator to LLVM (fresh, zeroed, 16-byte
/// aligned, no other effects, never unwinds), so it must stay exactly that.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_alloc_raw(size: i64) -> *mut u8 {
    alloc(size as usize)
}

/// Allocate memory the collector never scans, for a struct or enum variant with no reference
/// fields (numbers and bools only). Not zeroed: the generated code stores every field. Declared
/// to LLVM as an allocator of uninitialised memory, so it must stay free of other effects.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_alloc_atomic_raw(size: i64) -> *mut u8 {
    alloc_atomic(size as usize)
}

// ---- the inline allocation fast path (docs/09, phase 2) ----------------------------------------
//
// Generated code allocates small fixed-size objects without calling the runtime. Each thread has
// an `AllocCache`: for each size, a free list of zeroed objects linked through their first word.
// The code generator lowers `lrt_alloc_raw(size)` into: read the thread's cache pointer (a
// thread-local defined in the program's object, read with inline assembly so that LLVM never
// reuses the read after a fiber has moved to another thread), pop the list's head, and clear its
// link word. When the cache is missing or the list is empty, it calls `lrt_alloc_slow`, which
// refills the list from `GC_malloc_many`, as Boehm's `gc_inline.h` does.
//
// The cache itself comes from `GC_malloc_uncollectable`: the collector scans it, so every object
// on a free list stays reachable (through the head and the link words) and is never reclaimed
// while it waits there.

/// The largest size, in 16-byte granules, served from a thread's free lists. Must match
/// `ALLOC_CACHE_GRANULES` in `lang_codegen`.
pub const CACHE_GRANULES: usize = 16;

/// One thread's free lists, indexed by size in granules. Must match the layout the code
/// generator assumes: an array of `CACHE_GRANULES + 1` list heads.
#[repr(C)]
pub struct AllocCache {
    lists: [*mut u8; CACHE_GRANULES + 1],
}

/// The slow path of an inline allocation of `size` bytes, which needs `granules` granules
/// (`size` plus the collector's extra byte, rounded up to 16). `slot` is the calling thread's
/// cache pointer, which is null until the thread's first allocation. Returns zeroed memory, like
/// `lrt_alloc_raw`.
///
/// # Safety
/// `slot` is the calling thread's cache slot, and `granules` is at most `CACHE_GRANULES`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_alloc_slow(granules: i64, size: i64, slot: *mut *mut AllocCache) -> *mut u8 {
    // In stress mode nothing is cached, so that every allocation comes here and collects first.
    if STRESS.load(Ordering::Relaxed) {
        return alloc(size as usize);
    }
    let granules = granules as usize;
    debug_assert!((1..=CACHE_GRANULES).contains(&granules));
    // SAFETY: the caller passes this thread's slot.
    let mut cache = unsafe { *slot };
    if cache.is_null() {
        // SAFETY: plain allocation; the memory is zeroed, so every list starts empty.
        cache = unsafe { GC_malloc_uncollectable(std::mem::size_of::<AllocCache>()) }.cast();
        if cache.is_null() {
            out_of_memory(std::mem::size_of::<AllocCache>())
        }
        // SAFETY: as above.
        unsafe { *slot = cache };
    }
    // Asking for one byte less than the granules hold gives objects of exactly that size class,
    // whether or not the collector adds its extra byte for interior pointers.
    // SAFETY: plain allocation. The objects are zeroed except for the link in their first word.
    let first = unsafe { GC_malloc_many(granules * 16 - 1) };
    if first.is_null() {
        out_of_memory(size as usize)
    }
    // SAFETY: `first` heads a list of objects of at least 16 bytes, linked through word 0, and
    // `granules` indexes the cache's lists.
    unsafe {
        let next = *first.cast::<*mut u8>();
        (*cache).lists[granules] = next;
        *first.cast::<*mut u8>() = std::ptr::null_mut();
    }
    first
}
