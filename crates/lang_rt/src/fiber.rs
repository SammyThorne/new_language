//! Stage 2 concurrency: fibers on a pool of worker threads (docs/04-runtime-and-memory.md,
//! "Stage 2: M:N fibers").
//!
//! A fiber is a user-space thread with its own stack. Tasks (`spawn`) and HTTP connections run on
//! fibers, scheduled onto one worker thread per core. Waiting operations (channel, mutex, join,
//! sleep, socket reads and writes) park the fiber and switch to another, instead of blocking the
//! thread. The program's main function stays on the main thread, which is not a worker: when it
//! waits, it blocks as before, so a program that never spawns never starts a worker.
//!
//! Pieces:
//!
//! - **Context switch**: `lrt_fiber_switch` saves the callee-saved registers and the floating
//!   point control words on the current stack, stores the stack pointer, and loads another.
//! - **Stacks**: `mmap`ed reservations with a guard page below, committed as they are touched,
//!   and reused once a fiber has ended: each thread keeps a few, and moves them to and from a
//!   shared pool in batches. The fiber's record ([`Fiber`]) sits at the top of its own stack,
//!   so starting a fiber allocates nothing; references to it are counted ([`FiberRef`]), and
//!   the stack is reused when the last one goes.
//! - **Scheduler**: each worker has a run queue. Fibers made runnable by other threads go to a
//!   lock-free global queue ([`GlobalQueue`]). An idle worker takes from its own queue, then a
//!   share of the global one, then steals half of another worker's, then waits in `epoll_wait`
//!   on the shared poller, which also wakes it when work arrives from elsewhere.
//! - **Parking**: `park` / `unpark` with a permit, so a wake-up that races with going to sleep is
//!   never lost. [`WaitList`]s of waiters on the waiting fibers' stacks build the channels,
//!   mutexes and joins in `tasks.rs` on top.
//! - **Timers** and I/O deadlines: one timer thread (see `poll.rs`).
//!
//! **The collector.** Boehm scans each registered thread's stack from the stack pointer it
//! recorded when it stopped the thread to the "stack bottom" it was told about. A worker's stack
//! pointer may be on any fiber's stack, so the runtime scans workers itself, and nothing on the
//! context switch takes the collector's lock (docs/09, phase 3):
//!
//! - Boehm's "sp corrector" is called, once the world is stopped, with each thread's recorded
//!   stack pointer. For a worker, [`correct_sp`] keeps the pointer and replaces it with the
//!   worker's stack bottom, so Boehm itself scans an empty range.
//! - The "push other roots" hook, [`push_fibers`], then works out which stack each stopped
//!   worker was on from where its stack pointer is: inside the stack of the fiber the worker
//!   says it is running, or else on the worker's own stack. Each worker sets `running` before
//!   switching to a fiber and clears it after switching back, so whenever its stack pointer is
//!   on a fiber's stack, `running` is that fiber.
//! - For each worker it scans the live part of the stack it was on, and the live part of the
//!   other one from the stack pointer saved there: the scheduler's (`sched_sp`) while a fiber
//!   runs, or the fiber's (`Fiber::sp`) when the worker is on its own stack. The switch saves
//!   the stack pointer before it loads the other one, so whichever instruction a worker stopped
//!   at, both live parts are covered. The registers a stopped thread was using are pushed by
//!   Boehm's signal handler onto the stack it was on, below the recorded pointer.
//! - Every other fiber (parked, or queued) is scanned from its saved stack pointer up, which
//!   includes the registers the switch pushed. The list of fibers is changed under its own lock
//!   and read without it while the world is stopped, so it is changed in an order that keeps it
//!   readable after every store.
//!
//! **Panics** in a fiber unwind to the fiber's entry, which records the panic, releases the
//! mutexes the fiber held, and ends the fiber; the rest of the program carries on.

use crate::poll;
use std::cell::{Cell, UnsafeCell};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Instant;

// ---- the context switch ---------------------------------------------------------------

std::arch::global_asm!(
    r#"
    .text
    .p2align 4
    .globl lrt_fiber_switch
    .type lrt_fiber_switch,@function
lrt_fiber_switch:
    push rbp
    push rbx
    push r12
    push r13
    push r14
    push r15
    sub rsp, 8
    stmxcsr [rsp]
    fnstcw [rsp + 4]
    mov [rdi], rsp
    mov rsp, rsi
    ldmxcsr [rsp]
    fldcw [rsp + 4]
    add rsp, 8
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp
    ret
    .size lrt_fiber_switch, .-lrt_fiber_switch

    .p2align 4
    .globl lrt_fiber_start
    .type lrt_fiber_start,@function
lrt_fiber_start:
    .cfi_startproc
    .cfi_undefined rip
    mov rdi, r12
    call {main}
    ud2
    .cfi_endproc
    .size lrt_fiber_start, .-lrt_fiber_start
"#,
    main = sym fiber_main,
);

unsafe extern "C" {
    /// Save the current context, storing its stack pointer at `save`, and resume the context
    /// whose stack pointer is `next`.
    fn lrt_fiber_switch(save: *mut *mut u8, next: *mut u8);
    /// Where a new fiber starts: calls `fiber_main(r12)`.
    fn lrt_fiber_start();
}

// ---- the collector's hooks ------------------------------------------------------------

#[repr(C)]
struct StackBase {
    mem_base: *mut u8,
    reg_base: *mut u8,
}

type PushProc = Option<unsafe extern "C" fn()>;

type SpCorrector = Option<unsafe extern "C" fn(sp: *mut *mut u8, thread: usize)>;

#[link(name = "libgc.so.1", kind = "dylib", modifiers = "+verbatim")]
unsafe extern "C" {
    fn GC_get_my_stackbottom(base: *mut StackBase) -> *mut u8;
    fn GC_set_push_other_roots(proc_: PushProc);
    fn GC_get_push_other_roots() -> PushProc;
    fn GC_set_sp_corrector(proc_: SpCorrector);
    fn GC_get_sp_corrector() -> SpCorrector;
    fn GC_push_all_eager(bottom: *mut u8, top: *mut u8);
}

unsafe extern "C" {
    fn pthread_self() -> usize;
}

/// Every fiber stack the program has mapped, by slot, with the fiber using it (null for a
/// stack in a pool). A slot is set when a fiber starts and cleared when it ends, with single
/// stores and no lock; the collector reads the table while the world is stopped. Slots are
/// handed out in chunks that are never freed, so the table can be read at any moment.
struct StackTable {
    chunks: [AtomicPtr<AtomicPtr<Fiber>>; SLOT_CHUNKS],
    /// Slots handed out so far.
    count: AtomicUsize,
    /// Slots of stacks that were unmapped, for reuse.
    free: Mutex<Vec<usize>>,
}

const SLOT_CHUNK: usize = 4096;
const SLOT_CHUNKS: usize = 4096;

static STACKS: StackTable = StackTable {
    chunks: [const { AtomicPtr::new(std::ptr::null_mut()) }; SLOT_CHUNKS],
    count: AtomicUsize::new(0),
    free: Mutex::new(Vec::new()),
};

impl StackTable {
    /// The slot at `index`, if its chunk has been made.
    fn get(&self, index: usize) -> Option<&'static AtomicPtr<Fiber>> {
        let chunk = self.chunks.get(index / SLOT_CHUNK)?.load(Ordering::Acquire);
        // SAFETY: a chunk is SLOT_CHUNK slots, never freed.
        (!chunk.is_null()).then(|| unsafe { &*chunk.add(index % SLOT_CHUNK) })
    }

    fn slot(&self, index: usize) -> &'static AtomicPtr<Fiber> {
        self.get(index).expect("a stack's slot exists")
    }

    /// A slot for a newly mapped stack.
    fn new_slot(&self) -> usize {
        if let Some(index) = lock(&self.free).pop() {
            return index;
        }
        let index = self.count.fetch_add(1, Ordering::SeqCst);
        let Some(chunk) = self.chunks.get(index / SLOT_CHUNK) else {
            crate::out::flush();
            eprintln!("fatal: too many task stacks");
            std::process::abort();
        };
        if chunk.load(Ordering::Acquire).is_null() {
            let fresh: Box<[AtomicPtr<Fiber>]> =
                (0..SLOT_CHUNK).map(|_| AtomicPtr::new(std::ptr::null_mut())).collect();
            let fresh = Box::into_raw(fresh) as *mut AtomicPtr<Fiber>;
            if chunk.compare_exchange(std::ptr::null_mut(), fresh, Ordering::AcqRel, Ordering::Acquire).is_err() {
                // SAFETY: ours, never shared.
                drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(fresh, SLOT_CHUNK)) });
            }
        }
        index
    }
}

static PREVIOUS_PUSH: OnceLock<usize> = OnceLock::new();
static PREVIOUS_CORRECTOR: OnceLock<usize> = OnceLock::new();

/// Push `[bottom, top)` if it is a range.
unsafe fn push_range(bottom: *mut u8, top: *mut u8) {
    if !bottom.is_null() && bottom < top {
        unsafe { GC_push_all_eager(bottom, top) };
    }
}

/// The collector's "sp corrector", called with each stopped thread's stack pointer before its
/// stack is scanned. For a worker, keep the pointer for [`push_fibers`] and give Boehm an empty
/// range instead.
unsafe extern "C" fn correct_sp(sp: *mut *mut u8, thread: usize) {
    if let Some(&previous) = PREVIOUS_CORRECTOR.get()
        && previous != 0
    {
        // SAFETY: the corrector that was installed before ours.
        let previous: unsafe extern "C" fn(*mut *mut u8, usize) = unsafe { std::mem::transmute(previous) };
        unsafe { previous(sp, thread) };
    }
    let Some(scheduler) = SCHEDULER.get() else { return };
    for worker in &scheduler.workers {
        if worker.thread.load(Ordering::Acquire) == thread {
            // SAFETY: Boehm passes the stopped thread's recorded stack pointer.
            unsafe {
                worker.stopped_sp.store(*sp, Ordering::Relaxed);
                *sp = worker.native_bottom.load(Ordering::Relaxed);
            }
            return;
        }
    }
}

/// The collector's "push other roots" hook: the stacks of the workers and of every fiber, and
/// every fiber's references.
unsafe extern "C" fn push_fibers() {
    if let Some(&previous) = PREVIOUS_PUSH.get()
        && previous != 0
    {
        // SAFETY: the hook that was installed before ours, which scans the threads' stacks and
        // calls `correct_sp` for each.
        let previous: unsafe extern "C" fn() = unsafe { std::mem::transmute(previous) };
        unsafe { previous() };
    }
    let workers = SCHEDULER.get().map_or(&[][..], |s| &s.workers[..]);
    for worker in workers {
        // A worker whose thread is not yet registered with us is still on its own stack, which
        // Boehm scanned.
        let sp = worker.stopped_sp.swap(std::ptr::null_mut(), Ordering::Relaxed);
        if sp.is_null() {
            continue;
        }
        let native_top = worker.native_bottom.load(Ordering::Relaxed);
        let running = worker.running.load(Ordering::Acquire);
        // SAFETY: a worker's running fiber is alive: the worker holds a reference to it.
        let on_fiber = unsafe { running.as_ref() }.filter(|f| f.stack_lo <= sp && sp < f.stack_hi);
        unsafe {
            match on_fiber {
                Some(fiber) => {
                    push_range(sp, fiber.stack_hi);
                    push_range(*worker.sched_sp.get(), native_top);
                }
                None => {
                    if !(sp < native_top && native_top as usize - sp as usize <= NATIVE_STACK_LIMIT) {
                        crate::out::flush();
                        eprintln!("fatal: a worker thread stopped for collection on an unknown stack");
                        std::process::abort();
                    }
                    push_range(sp, native_top);
                    if let Some(fiber) = running.as_ref() {
                        push_range(*fiber.sp.get(), fiber.stack_hi);
                    }
                }
            }
        }
    }
    // The other fibers, from their saved stack pointers to the top of their stacks, which is
    // where their records (and so their references) are.
    for index in 0..STACKS.count.load(Ordering::SeqCst) {
        let Some(slot) = STACKS.get(index) else { continue };
        let fiber = slot.load(Ordering::Acquire);
        if fiber.is_null() || workers.iter().any(|w| std::ptr::eq(w.running.load(Ordering::Relaxed), fiber)) {
            continue;
        }
        // SAFETY: a fiber in the table is alive (it leaves the table before it can be freed).
        let f = unsafe { &*fiber };
        unsafe { push_range(*f.sp.get(), f.stack_hi) };
    }
}

fn install_gc_hook() {
    // SAFETY: plain configuration calls, made once before any fiber exists.
    PREVIOUS_PUSH.get_or_init(|| {
        let previous = unsafe { GC_get_push_other_roots() };
        unsafe { GC_set_push_other_roots(Some(push_fibers)) };
        previous.map_or(0, |p| p as usize)
    });
    PREVIOUS_CORRECTOR.get_or_init(|| {
        let previous = unsafe { GC_get_sp_corrector() };
        unsafe { GC_set_sp_corrector(Some(correct_sp)) };
        previous.map_or(0, |p| p as usize)
    });
}

// ---- stacks ---------------------------------------------------------------------------

const PAGE: usize = 4096;
const PROT_NONE: i32 = 0;
const PROT_READ_WRITE: i32 = 3;
const MAP_PRIVATE_ANONYMOUS_NORESERVE: i32 = 0x02 | 0x20 | 0x4000;

unsafe extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, offset: i64) -> *mut u8;
    fn munmap(addr: *mut u8, len: usize) -> i32;
    fn mprotect(addr: *mut u8, len: usize, prot: i32) -> i32;
}

struct Stack {
    /// The start of the mapping, which is the guard page.
    base: *mut u8,
    /// Bytes mapped, including the guard page.
    len: usize,
    /// The stack's slot in [`STACKS`].
    slot: usize,
}

// SAFETY: a stack is plain memory, used by one fiber at a time.
unsafe impl Send for Stack {}

/// Stacks no fiber uses, shared by all threads; each thread also keeps a few of its own.
static STACK_POOL: Mutex<Vec<Stack>> = Mutex::new(Vec::new());
const POOLED_STACKS: usize = 16384;
const CACHED_STACKS: usize = 64;
/// How many stacks a thread moves to or from the shared pool at once.
const STACK_BATCH: usize = 32;

thread_local! {
    static STACK_CACHE: UnsafeCell<Vec<Stack>> = const { UnsafeCell::new(Vec::new()) };
}

fn stack_size() -> usize {
    static SIZE: OnceLock<usize> = OnceLock::new();
    *SIZE.get_or_init(|| {
        let size = std::env::var(lang_names::env_var("TASK_STACK")).ok().and_then(|v| v.parse().ok());
        size.unwrap_or(1024 * 1024_usize).max(64 * 1024).next_multiple_of(PAGE)
    })
}

impl Stack {
    /// A stack from this thread's cache, the shared pool, or a new mapping. Not inlined: a
    /// thread-local address must not be kept across a wait (see [`current`]).
    #[inline(never)]
    fn new() -> Stack {
        let cached = STACK_CACHE.try_with(|cache| {
            // SAFETY: only this thread uses its cache, and not re-entrantly.
            let cache = unsafe { &mut *cache.get() };
            if cache.is_empty() {
                let mut pool = lock(&STACK_POOL);
                let from = pool.len().saturating_sub(STACK_BATCH);
                cache.extend(pool.drain(from..));
            }
            cache.pop()
        });
        if let Ok(Some(stack)) = cached {
            return stack;
        }
        let len = stack_size() + PAGE;
        // SAFETY: a fresh anonymous mapping; the lowest page becomes the guard.
        let base = unsafe { mmap(std::ptr::null_mut(), len, PROT_READ_WRITE, MAP_PRIVATE_ANONYMOUS_NORESERVE, -1, 0) };
        if base as isize == -1 || unsafe { mprotect(base, PAGE, PROT_NONE) } != 0 {
            crate::out::flush();
            eprintln!("fatal: could not allocate a stack for a task");
            std::process::abort();
        }
        Stack { base, len, slot: STACKS.new_slot() }
    }

    fn top(&self) -> *mut u8 {
        // SAFETY: one past the end of the mapping.
        unsafe { self.base.add(self.len) }
    }

    /// Return a stack that no fiber uses any more.
    #[inline(never)]
    fn release(self) {
        let mut stack = Some(self);
        let overflow = STACK_CACHE.try_with(|cache| {
            // SAFETY: as in `new`.
            let cache = unsafe { &mut *cache.get() };
            cache.extend(stack.take());
            if cache.len() > CACHED_STACKS {
                let from = cache.len() - STACK_BATCH;
                return cache.drain(from..).collect();
            }
            Vec::new()
        });
        // If the thread is exiting, straight to the pool.
        Stack::pool(overflow.unwrap_or_else(|_| stack.into_iter().collect()));
    }

    fn pool(stacks: Vec<Stack>) {
        if stacks.is_empty() {
            return;
        }
        let mut pool = lock(&STACK_POOL);
        let room = POOLED_STACKS.saturating_sub(pool.len());
        let mut stacks = stacks.into_iter();
        pool.extend(stacks.by_ref().take(room));
        drop(pool);
        for stack in stacks {
            // SAFETY: the mapping made in `new`, no longer used.
            unsafe { munmap(stack.base, stack.len) };
            lock(&STACKS.free).push(stack.slot);
        }
    }
}

// ---- fibers ---------------------------------------------------------------------------

const RUNNING: u8 = 0;
const PARKED: u8 = 1;
const QUEUED: u8 = 2;
const DONE: u8 = 3;

/// What a fiber asks of the worker when it switches out.
#[derive(Clone, Copy, PartialEq)]
enum Intent {
    Park,
    Exit,
}

/// References a fiber holds to collected memory, which the collector scans: the entry point's
/// environment and task, and the mutexes it holds (for releasing them if it panics).
const REFS: usize = 2 + MAX_HELD;
const MAX_HELD: usize = 14;

/// A fiber's record. It lives at the top of the fiber's own stack, so starting a fiber takes no
/// allocation, and the collector scans it with the stack. It is freed, and the stack reused,
/// when the last [`FiberRef`] goes.
#[repr(C)]
pub(crate) struct Fiber {
    /// The link in the global run queue; first, so a link is also its fiber.
    link: QueueLink,
    pub(crate) id: u64,
    references: AtomicUsize,
    state: AtomicU8,
    permit: AtomicBool,
    pub(crate) panicked: AtomicBool,
    /// The saved stack pointer while not running.
    sp: UnsafeCell<*mut u8>,
    /// The usable part of the stack, above the guard page.
    stack_lo: *mut u8,
    stack_hi: *mut u8,
    stack: Stack,
    intent: Cell<Intent>,
    entry: UnsafeCell<Option<Entry>>,
    /// See [`REFS`]: `refs[0..2]` are for the entry point, the rest the held mutexes.
    refs: UnsafeCell<[*mut u8; REFS]>,
    held: Cell<usize>,
}

// SAFETY: a fiber's mutable fields are touched only by the thread running it or by the worker
// switching it; the shared state is atomic.
unsafe impl Send for Fiber {}
unsafe impl Sync for Fiber {}

/// A counted reference to a fiber, like an `Arc` whose memory is the top of the fiber's stack.
pub(crate) struct FiberRef(std::ptr::NonNull<Fiber>);

// SAFETY: as for `Fiber`.
unsafe impl Send for FiberRef {}
unsafe impl Sync for FiberRef {}

impl FiberRef {
    /// Another reference to a fiber that is alive.
    fn to(fiber: &Fiber) -> FiberRef {
        fiber.references.fetch_add(1, Ordering::Relaxed);
        FiberRef(std::ptr::NonNull::from(fiber))
    }

    fn as_ptr(&self) -> *mut Fiber {
        self.0.as_ptr()
    }
}

impl Clone for FiberRef {
    fn clone(&self) -> FiberRef {
        FiberRef::to(self)
    }
}

impl std::ops::Deref for FiberRef {
    type Target = Fiber;
    fn deref(&self) -> &Fiber {
        // SAFETY: the reference keeps the fiber alive.
        unsafe { self.0.as_ref() }
    }
}

impl Drop for FiberRef {
    fn drop(&mut self) {
        if self.references.fetch_sub(1, Ordering::Release) != 1 {
            return;
        }
        std::sync::atomic::fence(Ordering::Acquire);
        // The last reference. The fiber has ended (a fiber that has not holds a reference to
        // itself: its worker's while it runs, a queue's while it is runnable, its waker's
        // while it waits), so nothing runs on the stack any more.
        // SAFETY: the record was written by `spawn` and is dropped once; `Stack` owns nothing
        // to drop, so it can be read out first.
        unsafe {
            let stack = std::ptr::read(&(*self.as_ptr()).stack);
            std::ptr::drop_in_place(self.as_ptr());
            stack.release();
        }
    }
}

/// What a fiber runs: a function with a word for it (a task), or a closure.
pub(crate) enum Entry {
    Plain(fn(&Fiber, usize), usize),
    Closure(Box<dyn FnOnce(&Fiber)>),
}

impl Fiber {
    /// The collected references given at spawn.
    pub(crate) fn refs(&self) -> (*mut u8, *mut u8) {
        // SAFETY: written before the fiber started; read only by the fiber itself.
        let refs = unsafe { &*self.refs.get() };
        (refs[0], refs[1])
    }

    /// Note that the fiber holds `mutex`, so a panic can release it.
    pub(crate) fn hold(&self, mutex: *mut u8) {
        let n = self.held.get();
        if n < MAX_HELD {
            // SAFETY: only the fiber itself changes its held list.
            unsafe { (*self.refs.get())[2 + n] = mutex };
        }
        self.held.set(n + 1);
    }

    pub(crate) fn release(&self, mutex: *mut u8) {
        let n = self.held.get().min(MAX_HELD);
        // SAFETY: as in `hold`.
        let refs = unsafe { &mut *self.refs.get() };
        if let Some(i) = (0..n).rev().find(|&i| refs[2 + i] == mutex) {
            refs.copy_within(3 + i..2 + n, 2 + i);
            refs[1 + n] = std::ptr::null_mut();
        }
        self.held.set(self.held.get().saturating_sub(1));
    }

    /// The mutexes still held, most recent first, emptying the list.
    fn take_held(&self) -> Vec<*mut u8> {
        let n = self.held.get().min(MAX_HELD);
        // SAFETY: as in `hold`.
        let refs = unsafe { &mut *self.refs.get() };
        let held = (0..n).rev().map(|i| std::mem::replace(&mut refs[2 + i], std::ptr::null_mut())).collect();
        self.held.set(0);
        held
    }
}

static NEXT_FIBER: AtomicU64 = AtomicU64::new(1);

/// Start a fiber running `entry`. `refs` are collected references the entry needs, kept alive
/// until the fiber ends.
pub(crate) fn spawn(refs: [*mut u8; 2], entry: Entry) -> FiberRef {
    start_workers();
    let stack = Stack::new();
    let top = stack.top();
    // SAFETY: the first page is the guard.
    let bottom = unsafe { stack.base.add(PAGE) };
    let slot = STACKS.slot(stack.slot);
    let mut all_refs = [std::ptr::null_mut(); REFS];
    all_refs[..2].copy_from_slice(&refs);
    // The record goes at the top of the stack, aligned to a cache line; the first frame below.
    let record = ((top as usize - std::mem::size_of::<Fiber>()) & !63) as *mut Fiber;
    let fiber = Fiber {
        link: QueueLink { next: AtomicPtr::new(std::ptr::null_mut()) },
        id: NEXT_FIBER.fetch_add(1, Ordering::Relaxed),
        // The one returned.
        references: AtomicUsize::new(1),
        state: AtomicU8::new(QUEUED),
        permit: AtomicBool::new(false),
        panicked: AtomicBool::new(false),
        sp: UnsafeCell::new(std::ptr::null_mut()),
        stack_lo: bottom,
        stack_hi: top,
        stack,
        intent: Cell::new(Intent::Exit),
        entry: UnsafeCell::new(Some(entry)),
        refs: UnsafeCell::new(all_refs),
        held: Cell::new(0),
    };
    // SAFETY: the top of a stack no fiber uses, with room for the record.
    let fiber = unsafe {
        record.write(fiber);
        FiberRef(std::ptr::NonNull::new_unchecked(record))
    };
    // The first switch "returns" into `lrt_fiber_start` with the fiber in r12 (see the layout
    // in `lrt_fiber_switch`): control words, r15 to r12, rbx, rbp, return address, and a zero
    // word where the stack is 16-byte aligned, as a call expects.
    // SAFETY: writing the initial frame in the 80 bytes below the record.
    unsafe {
        let words = record as *mut u64;
        words.sub(1).write(0);
        words.sub(2).write(0);
        words.sub(3).write(lrt_fiber_start as *const () as u64);
        words.sub(4).write(0); // rbp
        words.sub(5).write(0); // rbx
        words.sub(6).write(fiber.as_ptr() as u64); // r12
        words.sub(7).write(0); // r13
        words.sub(8).write(0); // r14
        words.sub(9).write(0); // r15
        words.sub(10).write(0x037F_0000_1F80); // MXCSR default, then the x87 control word
        *fiber.sp.get() = words.sub(10) as *mut u8;
    }
    // From here on the collector scans the stack, and the record with it.
    slot.store(fiber.as_ptr(), Ordering::Release);
    enqueue(fiber.clone());
    fiber
}

/// The first thing a fiber runs, on its own stack.
extern "C" fn fiber_main(fiber: *const Fiber) -> ! {
    // SAFETY: the worker running this fiber holds a reference to it.
    let fiber = unsafe { &*fiber };
    // SAFETY: only this fiber takes its entry, once.
    let entry = unsafe { (*fiber.entry.get()).take() };
    if let Some(entry) = entry {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match entry {
            Entry::Plain(function, word) => function(fiber, word),
            Entry::Closure(closure) => closure(fiber),
        }));
        if outcome.is_err() {
            fiber.panicked.store(true, Ordering::SeqCst);
        }
    }
    release_held(fiber);
    suspend(Intent::Exit);
    unreachable!("an ended fiber was resumed")
}

/// Release the mutexes a panicking fiber still holds, as the `with` blocks it left would have.
pub(crate) fn release_held(fiber: &Fiber) {
    for mutex in fiber.take_held() {
        crate::tasks::unlock_for_panic(mutex);
    }
}

// ---- the current fiber ------------------------------------------------------------------

thread_local! {
    static CURRENT: Cell<*const Fiber> = const { Cell::new(std::ptr::null()) };
    static WORKER: Cell<*const Worker> = const { Cell::new(std::ptr::null()) };
}

/// The fiber running on this thread, if any. Not inlined: a fiber can move between threads at
/// any wait, so a thread-local address must never be kept across one.
#[inline(never)]
pub(crate) fn current() -> Option<&'static Fiber> {
    let fiber = CURRENT.with(Cell::get);
    // SAFETY: the running fiber outlives the code it runs.
    unsafe { fiber.as_ref() }
}

#[inline(never)]
fn current_worker() -> *const Worker {
    WORKER.with(Cell::get)
}

/// The addresses that count as running off the end of the running fiber's stack: its guard
/// page and the space beside it. Safe to call from a signal handler.
pub(crate) fn guard_window() -> Option<(usize, usize)> {
    let fiber = current()?;
    let base = fiber.stack.base as usize;
    Some((base.saturating_sub(64 * 1024), base + PAGE))
}

/// A reference to the running fiber that can outlive this call.
fn current_ref() -> Option<FiberRef> {
    Some(FiberRef::to(current()?))
}

/// Switch from the running fiber to its worker's scheduler.
#[inline(never)]
fn suspend(intent: Intent) {
    let fiber = current().expect("suspend outside a fiber");
    let worker = current_worker();
    fiber.intent.set(intent);
    // SAFETY: the worker resumes from its saved context.
    unsafe { lrt_fiber_switch(fiber.sp.get(), *(*worker).sched_sp.get()) };
}

// ---- parking ----------------------------------------------------------------------------

/// Something to wake: a fiber, or a thread that is not running fibers (the main thread).
#[derive(Clone)]
pub(crate) enum Waker {
    Fiber(FiberRef),
    Thread(std::thread::Thread),
}

impl Waker {
    pub(crate) fn current() -> Waker {
        match current_ref() {
            Some(fiber) => Waker::Fiber(fiber),
            None => Waker::Thread(std::thread::current()),
        }
    }

    pub(crate) fn wake(&self) {
        match self {
            Waker::Fiber(fiber) => unpark(fiber),
            Waker::Thread(thread) => thread.unpark(),
        }
    }
}

/// Wait until woken, or, if `deadline` is given, until then. May return early: callers check
/// what they are waiting for and wait again.
pub(crate) fn park(deadline: Option<Instant>) {
    match current() {
        Some(fiber) => {
            if fiber.permit.swap(false, Ordering::SeqCst) {
                return;
            }
            if let Some(deadline) = deadline {
                poll::add_timer(deadline, current_ref().expect("in a fiber"));
            }
            suspend(Intent::Park);
            fiber.permit.store(false, Ordering::SeqCst);
        }
        None => match deadline {
            Some(deadline) => std::thread::park_timeout(deadline.saturating_duration_since(Instant::now())),
            None => std::thread::park(),
        },
    }
}

pub(crate) fn unpark(fiber: &FiberRef) {
    fiber.permit.store(true, Ordering::SeqCst);
    if fiber.state.compare_exchange(PARKED, QUEUED, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
        enqueue(fiber.clone());
    }
}

/// A waiter on a [`WaitList`]: it lives on the waiting task's stack while it waits.
pub(crate) struct WaitNode {
    waker: Waker,
    signaled: AtomicBool,
    next: Cell<*const WaitNode>,
    prev: Cell<*const WaitNode>,
}

impl WaitNode {
    pub(crate) fn new() -> WaitNode {
        WaitNode {
            waker: Waker::current(),
            signaled: AtomicBool::new(false),
            next: Cell::new(std::ptr::null()),
            prev: Cell::new(std::ptr::null()),
        }
    }

    pub(crate) fn signaled(&self) -> bool {
        self.signaled.load(Ordering::Acquire)
    }

    /// Wait until signaled.
    pub(crate) fn wait(&self) {
        while !self.signaled() {
            park(None);
        }
    }
}

/// A queue of waiters, kept inside the object they wait on and protected by that object's lock.
/// The nodes are on the waiters' stacks; a node is unlinked before it is signaled or before its
/// waiter gives up, so the list never points at a stack frame that has gone.
#[repr(C)]
pub(crate) struct WaitList {
    head: *const WaitNode,
    tail: *const WaitNode,
}

impl WaitList {
    pub(crate) const fn new() -> WaitList {
        WaitList { head: std::ptr::null(), tail: std::ptr::null() }
    }

    /// Add a waiter. It must stay where it is until signaled or removed.
    pub(crate) fn push(&mut self, node: &WaitNode) {
        node.next.set(std::ptr::null());
        node.prev.set(self.tail);
        if self.tail.is_null() {
            self.head = node;
        } else {
            // SAFETY: linked nodes are alive (see above).
            unsafe { (*self.tail).next.set(node) };
        }
        self.tail = node;
    }

    /// Remove a waiter that gave up (a timeout), if it is still linked.
    pub(crate) fn remove(&mut self, node: &WaitNode) {
        if node.signaled() {
            return;
        }
        let (prev, next) = (node.prev.get(), node.next.get());
        // SAFETY: linked nodes are alive.
        unsafe {
            if prev.is_null() {
                if std::ptr::eq(self.head, node) {
                    self.head = next;
                }
            } else {
                (*prev).next.set(next);
            }
            if next.is_null() {
                if std::ptr::eq(self.tail, node) {
                    self.tail = prev;
                }
            } else {
                (*next).prev.set(prev);
            }
        }
    }

    /// Unlink the first waiter and signal it. False if there was none.
    pub(crate) fn wake_one(&mut self) -> bool {
        let node = self.head;
        if node.is_null() {
            return false;
        }
        // SAFETY: linked nodes are alive until signaled.
        unsafe {
            self.head = (*node).next.get();
            if self.head.is_null() {
                self.tail = std::ptr::null();
            } else {
                (*self.head).prev.set(std::ptr::null());
            }
            // Copy the waker first: once signaled, the waiter may return and its node go away.
            let waker = (*node).waker.clone();
            (*node).signaled.store(true, Ordering::Release);
            waker.wake();
        }
        true
    }

    pub(crate) fn wake_all(&mut self) {
        while self.wake_one() {}
    }
}

/// A lock that parks fibers rather than blocking threads, for runtime state that is held
/// across a wait (a WebSocket's reader and writer).
pub(crate) struct FiberLock<T> {
    state: Mutex<(bool, WaitList)>,
    value: UnsafeCell<T>,
}

// SAFETY: the value is only reached through a guard, which the lock makes exclusive.
unsafe impl<T: Send> Sync for FiberLock<T> {}
unsafe impl<T: Send> Send for FiberLock<T> {}

pub(crate) struct FiberGuard<'a, T> {
    lock: &'a FiberLock<T>,
}

impl<T> FiberLock<T> {
    pub(crate) fn new(value: T) -> FiberLock<T> {
        FiberLock { state: Mutex::new((false, WaitList::new())), value: UnsafeCell::new(value) }
    }

    pub(crate) fn lock(&self) -> FiberGuard<'_, T> {
        loop {
            let node = WaitNode::new();
            {
                let mut state = lock(&self.state);
                if !state.0 {
                    state.0 = true;
                    return FiberGuard { lock: self };
                }
                state.1.push(&node);
            }
            node.wait();
        }
    }
}

impl<T> std::ops::Deref for FiberGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: the guard holds the lock.
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> std::ops::DerefMut for FiberGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: the guard holds the lock.
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T> Drop for FiberGuard<'_, T> {
    fn drop(&mut self) {
        let mut state = lock(&self.lock.state);
        state.0 = false;
        state.1.wake_one();
    }
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

// ---- the scheduler ----------------------------------------------------------------------

/// A queue of runnable fibers. Its length is kept outside the lock too, so that looking for
/// work does not take the locks of empty queues.
struct RunQueue {
    items: Mutex<VecDeque<FiberRef>>,
    len: AtomicUsize,
}

impl RunQueue {
    fn new() -> RunQueue {
        RunQueue { items: Mutex::new(VecDeque::new()), len: AtomicUsize::new(0) }
    }

    fn is_empty(&self) -> bool {
        self.len.load(Ordering::SeqCst) == 0
    }

    /// Add a fiber; true if the queue was empty.
    fn push(&self, fiber: FiberRef) -> bool {
        let mut items = lock(&self.items);
        items.push_back(fiber);
        self.len.store(items.len(), Ordering::SeqCst);
        items.len() == 1
    }

    fn extend(&self, fibers: impl IntoIterator<Item = FiberRef>) {
        let mut items = lock(&self.items);
        items.extend(fibers);
        self.len.store(items.len(), Ordering::SeqCst);
    }

    /// Up to `limit` fibers from the front, in order; none if the lock is busy and `wait` is
    /// false.
    fn take(&self, limit: impl FnOnce(usize) -> usize, wait: bool) -> VecDeque<FiberRef> {
        if self.is_empty() {
            return VecDeque::new();
        }
        let mut items = if wait {
            lock(&self.items)
        } else {
            match self.items.try_lock() {
                Ok(items) => items,
                Err(_) => return VecDeque::new(),
            }
        };
        let take = limit(items.len()).min(items.len());
        let taken = if take == items.len() { std::mem::take(&mut *items) } else { items.drain(..take).collect() };
        self.len.store(items.len(), Ordering::SeqCst);
        taken
    }

    fn pop(&self) -> Option<FiberRef> {
        if self.is_empty() {
            return None;
        }
        let mut items = lock(&self.items);
        let fiber = items.pop_front();
        self.len.store(items.len(), Ordering::SeqCst);
        fiber
    }
}

/// A link in [`GlobalQueue`].
struct QueueLink {
    next: AtomicPtr<QueueLink>,
}

/// The queue of fibers made runnable by threads that are not workers (the main thread, the
/// accept loop, the timer thread): an intrusive multi-producer queue (Vyukov's), so adding a
/// fiber is an atomic swap and a store, never a lock, however many workers are taking from it.
/// One worker at a time takes (`taking`); the others look elsewhere meanwhile.
///
/// The producers' fields and the taker's are on separate cache lines, and the queue on lines of
/// its own, away from other hot statics.
#[repr(C, align(128))]
struct GlobalQueue {
    /// The link added last.
    head: AtomicPtr<QueueLink>,
    /// Fibers added or being added and not yet taken.
    len: AtomicUsize,
    taker: CacheLine<Taker>,
}

struct Taker {
    /// The next link to take, or the stub; only the taker touches it.
    tail: UnsafeCell<*mut QueueLink>,
    taking: AtomicBool,
}

#[repr(align(128))]
struct CacheLine<T>(T);

impl<T> std::ops::Deref for CacheLine<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

// SAFETY: `tail` is only used by the worker holding `taking`; the rest is atomic.
unsafe impl Sync for GlobalQueue {}

/// The queue's permanent link, which keeps it from ever being empty of links.
static STUB: QueueLink = QueueLink { next: AtomicPtr::new(std::ptr::null_mut()) };

static GLOBAL: GlobalQueue = GlobalQueue {
    head: AtomicPtr::new(std::ptr::addr_of!(STUB) as *mut QueueLink),
    len: AtomicUsize::new(0),
    taker: CacheLine(Taker {
        tail: UnsafeCell::new(std::ptr::addr_of!(STUB) as *mut QueueLink),
        taking: AtomicBool::new(false),
    }),
};

impl GlobalQueue {
    fn is_empty(&self) -> bool {
        self.len.load(Ordering::SeqCst) == 0
    }

    /// Whether a worker could take from the queue now. While one is taking, the others need
    /// not look: the taker wakes another if it leaves fibers behind.
    fn takeable(&self) -> bool {
        !self.is_empty() && !self.taker.taking.load(Ordering::SeqCst)
    }

    /// Add a fiber; true if the queue was empty.
    fn push(&self, fiber: FiberRef) -> bool {
        // Counted first, so the count is never below the links a taker can find.
        let was_empty = self.len.fetch_add(1, Ordering::SeqCst) == 0;
        // The queue owns the reference until the fiber is taken.
        let link = fiber.as_ptr() as *mut QueueLink;
        std::mem::forget(fiber);
        // SAFETY: the link is the fiber's own, unused while it is not queued.
        unsafe { self.append(link) };
        was_empty
    }

    /// # Safety
    /// `link` is not in the queue.
    unsafe fn append(&self, link: *mut QueueLink) {
        unsafe {
            (*link).next.store(std::ptr::null_mut(), Ordering::Relaxed);
            let previous = self.head.swap(link, Ordering::AcqRel);
            (*previous).next.store(link, Ordering::Release);
        }
    }

    /// Take the next fiber. None if the queue is empty, or if the fiber added next is still
    /// being linked by its producer.
    ///
    /// # Safety
    /// The caller holds `taking`.
    unsafe fn pop(&self) -> Option<FiberRef> {
        let stub = std::ptr::addr_of!(STUB) as *mut QueueLink;
        unsafe {
            let tail = &mut *self.taker.tail.get();
            let mut next = (**tail).next.load(Ordering::Acquire);
            if *tail == stub {
                if next.is_null() {
                    return None;
                }
                *tail = next;
                next = (*next).next.load(Ordering::Acquire);
            }
            let taken = *tail;
            if next.is_null() {
                // `taken` may be the last link. If a producer is adding after it, wait for
                // that; otherwise put the stub behind it so it can be taken.
                if taken != self.head.load(Ordering::Acquire) {
                    return None;
                }
                self.append(stub);
                next = (*taken).next.load(Ordering::Acquire);
                if next.is_null() {
                    return None;
                }
            }
            *tail = next;
            self.len.fetch_sub(1, Ordering::SeqCst);
            Some(FiberRef(std::ptr::NonNull::new_unchecked(taken as *mut Fiber)))
        }
    }

    /// Up to `limit(len)` fibers, in order; none if another worker is taking.
    fn take(&self, limit: impl FnOnce(usize) -> usize) -> VecDeque<FiberRef> {
        let len = self.len.load(Ordering::SeqCst);
        if len == 0 || self.taker.taking.swap(true, Ordering::SeqCst) {
            return VecDeque::new();
        }
        let mut taken = VecDeque::new();
        let want = limit(len).max(1);
        let mut waits = 0;
        while taken.len() < want {
            // SAFETY: this worker holds `taking`.
            match unsafe { self.pop() } {
                Some(fiber) => taken.push_back(fiber),
                // A producer between its two steps finishes within a few instructions,
                // unless it was preempted; then leave the rest for later.
                None if taken.is_empty() && !self.is_empty() && waits < 100 => {
                    waits += 1;
                    std::hint::spin_loop();
                }
                None => break,
            }
        }
        self.taker.taking.store(false, Ordering::SeqCst);
        taken
    }
}

struct Worker {
    queue: RunQueue,
    /// The scheduler's saved stack pointer while a fiber runs.
    sched_sp: UnsafeCell<*mut u8>,
    /// The fiber the worker is switching to, running, or switching back from.
    running: AtomicPtr<Fiber>,
    /// The worker's thread (`pthread_t`), once it has started.
    thread: AtomicUsize,
    /// The top of the worker thread's own stack, which Boehm calls its bottom.
    native_bottom: AtomicPtr<u8>,
    /// Where the worker was stopped for the collection in progress (see `correct_sp`).
    stopped_sp: AtomicPtr<u8>,
}

/// The most a worker thread's own stack can hold: its size and a margin.
const NATIVE_STACK: usize = 256 * 1024;
const NATIVE_STACK_LIMIT: usize = NATIVE_STACK + 64 * 1024;

// SAFETY: `sched_sp` is written only by the worker's own thread, and read by the collector
// while the world is stopped.
unsafe impl Sync for Worker {}
unsafe impl Send for Worker {}

struct Scheduler {
    workers: Vec<Worker>,
    /// Workers waiting in the poller.
    idle: AtomicUsize,
    /// A wake-up has been sent and not yet taken.
    waking: AtomicBool,
}

static SCHEDULER: OnceLock<Scheduler> = OnceLock::new();

fn worker_count() -> usize {
    let configured = std::env::var(lang_names::env_var("WORKERS")).ok().and_then(|v| v.parse::<usize>().ok());
    configured.unwrap_or_else(|| std::thread::available_parallelism().map_or(4, |n| n.get())).clamp(1, 1024)
}

/// Start the workers, on the first `spawn` or `serve` (always on the main thread first, since
/// no other thread exists before).
pub(crate) fn start_workers() {
    if SCHEDULER.get().is_some() {
        return;
    }
    crate::tasks::allow_threads();
    install_gc_hook();
    poll::init();
    let count = worker_count();
    let scheduler = Scheduler {
        workers: (0..count)
            .map(|_| Worker {
                queue: RunQueue::new(),
                sched_sp: UnsafeCell::new(std::ptr::null_mut()),
                running: AtomicPtr::new(std::ptr::null_mut()),
                thread: AtomicUsize::new(0),
                native_bottom: AtomicPtr::new(std::ptr::null_mut()),
                stopped_sp: AtomicPtr::new(std::ptr::null_mut()),
            })
            .collect(),
        idle: AtomicUsize::new(0),
        waking: AtomicBool::new(false),
    };
    if SCHEDULER.set(scheduler).is_err() {
        return;
    }
    for index in 0..count {
        let started = std::thread::Builder::new()
            .name(format!("worker-{index}"))
            .stack_size(NATIVE_STACK)
            .spawn(move || worker_main(index));
        if let Err(error) = started {
            crate::out::flush();
            eprintln!("fatal: could not start a worker thread: {error}");
            std::process::abort();
        }
    }
    poll::start_timer_thread();
}

fn scheduler() -> &'static Scheduler {
    SCHEDULER.get().expect("the scheduler is running")
}

/// Make a fiber runnable: on this worker's queue if called from a worker, else the global one.
fn enqueue(fiber: FiberRef) {
    let worker = current_worker();
    if worker.is_null() {
        // A worker is woken when the global queue stops being empty. Until it is empty again,
        // each worker that takes from it and leaves some behind wakes the next (`find_work`),
        // so a thread starting many tasks makes one wake-up, not one per task.
        if GLOBAL.push(fiber) {
            wake_idle_worker();
        }
    } else {
        // SAFETY: the worker lives as long as the scheduler.
        unsafe { &(*worker).queue }.push(fiber);
        wake_idle_worker();
    }
}

/// The poller has taken a wake-up (whichever worker took it): the next one may be sent.
pub(crate) fn wake_taken() {
    if let Some(scheduler) = SCHEDULER.get() {
        scheduler.waking.store(false, Ordering::SeqCst);
    }
}

/// Wake a worker waiting in the poller.
///
/// Against a lost wake-up: a worker counts itself idle, then looks at the queues once more
/// before it waits; this side adds to a queue, then reads `idle`. Both steps are sequentially
/// consistent, so either the worker sees the new fiber or this side sees it idle.
fn wake_idle_worker() {
    let Some(scheduler) = SCHEDULER.get() else { return };
    if scheduler.idle.load(Ordering::SeqCst) > 0
        && scheduler.waking.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_ok()
    {
        poll::notify();
    }
}

fn worker_main(index: usize) {
    crate::tasks::register_thread();
    crate::overflow::setup_thread();
    let scheduler = scheduler();
    let worker = &scheduler.workers[index];
    WORKER.with(|w| w.set(worker));
    let mut base = StackBase { mem_base: std::ptr::null_mut(), reg_base: std::ptr::null_mut() };
    // SAFETY: the calling thread is registered.
    unsafe { GC_get_my_stackbottom(&mut base) };
    worker.native_bottom.store(base.mem_base, Ordering::Relaxed);
    // From here on the collector leaves this thread's stacks to `push_fibers`.
    // SAFETY: a plain call.
    worker.thread.store(unsafe { pthread_self() }, Ordering::Release);
    let mut ticks: u32 = 0;
    loop {
        ticks = ticks.wrapping_add(1);
        if ticks % 61 == 0 {
            // Busy workers still look at the network now and then.
            poll::poll(0);
        }
        if let Some(fiber) = find_work(scheduler, index) {
            run(worker, fiber);
            continue;
        }
        scheduler.idle.fetch_add(1, Ordering::SeqCst);
        if has_work(scheduler) {
            scheduler.idle.fetch_sub(1, Ordering::SeqCst);
            continue;
        }
        poll::poll(-1);
        scheduler.idle.fetch_sub(1, Ordering::SeqCst);
    }
}

fn has_work(scheduler: &Scheduler) -> bool {
    GLOBAL.takeable() || scheduler.workers.iter().any(|w| !w.queue.is_empty())
}

fn find_work(scheduler: &Scheduler, index: usize) -> Option<FiberRef> {
    let own = &scheduler.workers[index].queue;
    if let Some(fiber) = own.pop() {
        return Some(fiber);
    }
    // From the global queue, a fair share at once, so that a thread starting many tasks is
    // not slowed by every worker taking its lock for each one.
    let count = scheduler.workers.len();
    let mut taken = GLOBAL.take(|len| (len / count + 1).min(64));
    if !GLOBAL.is_empty() {
        wake_idle_worker();
    }
    if taken.is_empty() {
        // Steal half of another worker's queue.
        for offset in 1..count {
            let victim = &scheduler.workers[(index + offset) % count];
            taken = victim.queue.take(|len| len.div_ceil(2), false);
            if !taken.is_empty() {
                break;
            }
        }
    }
    let first = taken.pop_front();
    if !taken.is_empty() {
        own.extend(taken);
        wake_idle_worker();
    }
    first
}

/// Run a fiber until it switches out, then do what it asked.
fn run(worker: &Worker, fiber: FiberRef) {
    fiber.state.store(RUNNING, Ordering::SeqCst);
    CURRENT.with(|c| c.set(fiber.as_ptr()));
    // See "The collector" above: `running` covers the whole switch, both ways.
    worker.running.store(fiber.as_ptr(), Ordering::Release);
    // SAFETY: the fiber's saved context, made by `spawn` or by its last switch out.
    unsafe { lrt_fiber_switch(worker.sched_sp.get(), *fiber.sp.get()) };
    worker.running.store(std::ptr::null_mut(), Ordering::Release);
    if fiber.intent.get() == Intent::Exit {
        STACKS.slot(fiber.stack.slot).store(std::ptr::null_mut(), Ordering::Release);
    }
    CURRENT.with(|c| c.set(std::ptr::null()));
    match fiber.intent.get() {
        Intent::Park => {
            fiber.state.store(PARKED, Ordering::SeqCst);
            if fiber.permit.load(Ordering::SeqCst)
                && fiber.state.compare_exchange(PARKED, QUEUED, Ordering::SeqCst, Ordering::SeqCst).is_ok()
            {
                enqueue(fiber);
            }
        }
        Intent::Exit => {
            // The stack is reused once the last reference goes, which may be this one.
            fiber.state.store(DONE, Ordering::SeqCst);
        }
    }
}
