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
//!   and pooled when a fiber ends.
//! - **Scheduler**: each worker has a run queue; an idle worker takes from the global queue, then
//!   steals from the others, then waits in `epoll_wait` on the shared poller, which also wakes
//!   it when work arrives from elsewhere.
//! - **Parking**: `park` / `unpark` with a permit, so a wake-up that races with going to sleep is
//!   never lost. [`WaitList`]s of waiters on the waiting fibers' stacks build the channels,
//!   mutexes and joins in `tasks.rs` on top.
//! - **Timers** and I/O deadlines: one timer thread (see `poll.rs`).
//!
//! **The collector.** Boehm scans each registered thread's stack from its stack pointer to the
//! "stack bottom" it was told about. When a worker switches to a fiber, it sets that bottom to
//! the top of the fiber's stack, and back when the fiber switches out; both happen while holding
//! the collector's allocation lock, so a collection never sees a half-done switch. Fibers that
//! are not running (parked, or queued) are reported through Boehm's "push other roots" hook:
//! the live part of each stack, from its saved stack pointer up, which includes the registers
//! the switch pushed. The list of fibers is only changed under the allocation lock, which the
//! collector holds while it marks.
//!
//! **Panics** in a fiber unwind to the fiber's entry, which records the panic, releases the
//! mutexes the fiber held, and ends the fiber; the rest of the program carries on.

use crate::poll;
use std::cell::{Cell, UnsafeCell};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
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

#[link(name = "libgc.so.1", kind = "dylib", modifiers = "+verbatim")]
unsafe extern "C" {
    fn GC_alloc_lock();
    fn GC_alloc_unlock();
    fn GC_get_my_stackbottom(base: *mut StackBase) -> *mut u8;
    fn GC_set_stackbottom(thread: *mut u8, base: *const StackBase);
    fn GC_set_push_other_roots(proc_: PushProc);
    fn GC_get_push_other_roots() -> PushProc;
    fn GC_push_all_eager(bottom: *mut u8, top: *mut u8);
}

/// The fibers that exist, linked through `Fiber::next`/`prev`. Changed only while holding the
/// collector's allocation lock.
struct Registry(UnsafeCell<*const Fiber>);
// SAFETY: accessed only under the collector's allocation lock (see above).
unsafe impl Sync for Registry {}
static REGISTRY: Registry = Registry(UnsafeCell::new(std::ptr::null()));

static PREVIOUS_PUSH: OnceLock<usize> = OnceLock::new();

/// The collector's "push other roots" hook: every fiber's references, and the stacks of the
/// fibers that are not running on a thread.
unsafe extern "C" fn push_fibers() {
    if let Some(&previous) = PREVIOUS_PUSH.get()
        && previous != 0
    {
        // SAFETY: the hook that was installed before ours.
        let previous: unsafe extern "C" fn() = unsafe { std::mem::transmute(previous) };
        unsafe { previous() };
    }
    // SAFETY: the collector holds the allocation lock while it marks, so the list is stable.
    let mut fiber = unsafe { *REGISTRY.0.get() };
    while !fiber.is_null() {
        let f = unsafe { &*fiber };
        let refs = f.refs.get() as *mut u8;
        unsafe { GC_push_all_eager(refs, refs.add(std::mem::size_of::<[*mut u8; REFS]>())) };
        if !unsafe { *f.on_cpu.get() } {
            let sp = unsafe { *f.sp.get() };
            if !sp.is_null() {
                unsafe { GC_push_all_eager(sp, f.stack_hi) };
            }
        }
        fiber = unsafe { *f.next.get() };
    }
}

fn install_gc_hook() {
    // SAFETY: plain configuration calls, made once before any fiber exists.
    PREVIOUS_PUSH.get_or_init(|| {
        let previous = unsafe { GC_get_push_other_roots() };
        unsafe { GC_set_push_other_roots(Some(push_fibers)) };
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
}

// SAFETY: a stack is plain memory, used by one fiber at a time.
unsafe impl Send for Stack {}

static STACK_POOL: Mutex<Vec<Stack>> = Mutex::new(Vec::new());
const POOLED_STACKS: usize = 16384;

fn stack_size() -> usize {
    static SIZE: OnceLock<usize> = OnceLock::new();
    *SIZE.get_or_init(|| {
        let size = std::env::var(lang_names::env_var("TASK_STACK")).ok().and_then(|v| v.parse().ok());
        size.unwrap_or(1024 * 1024_usize).max(64 * 1024).next_multiple_of(PAGE)
    })
}

impl Stack {
    fn new() -> Stack {
        if let Some(stack) = lock(&STACK_POOL).pop() {
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
        Stack { base, len }
    }

    fn top(&self) -> *mut u8 {
        // SAFETY: one past the end of the mapping.
        unsafe { self.base.add(self.len) }
    }

    fn release(self) {
        let mut pool = lock(&STACK_POOL);
        if pool.len() < POOLED_STACKS {
            pool.push(self);
        } else {
            drop(pool);
            // SAFETY: the mapping made in `new`, no longer used.
            unsafe { munmap(self.base, self.len) };
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

pub(crate) struct Fiber {
    pub(crate) id: u64,
    state: AtomicU8,
    permit: AtomicBool,
    pub(crate) panicked: AtomicBool,
    /// The saved stack pointer while not running.
    sp: UnsafeCell<*mut u8>,
    stack_hi: *mut u8,
    stack: UnsafeCell<Option<Stack>>,
    /// Whether a worker is running it; changed under the allocation lock.
    on_cpu: UnsafeCell<bool>,
    intent: Cell<Intent>,
    entry: UnsafeCell<Option<Entry>>,
    /// See [`REFS`]: `refs[0..2]` are for the entry point, the rest the held mutexes.
    refs: UnsafeCell<[*mut u8; REFS]>,
    held: Cell<usize>,
    next: UnsafeCell<*const Fiber>,
    prev: UnsafeCell<*const Fiber>,
}

// SAFETY: a fiber's mutable fields are touched only by the thread running it, by the worker
// switching it, or under the allocation lock; the shared state is atomic.
unsafe impl Send for Fiber {}
unsafe impl Sync for Fiber {}

pub(crate) type FiberRef = Arc<Fiber>;

/// What a fiber runs.
pub(crate) type Entry = Box<dyn FnOnce(&Fiber)>;

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
    let mut all_refs = [std::ptr::null_mut(); REFS];
    all_refs[..2].copy_from_slice(&refs);
    let fiber = Arc::new(Fiber {
        id: NEXT_FIBER.fetch_add(1, Ordering::Relaxed),
        state: AtomicU8::new(QUEUED),
        permit: AtomicBool::new(false),
        panicked: AtomicBool::new(false),
        sp: UnsafeCell::new(std::ptr::null_mut()),
        stack_hi: top,
        stack: UnsafeCell::new(Some(stack)),
        on_cpu: UnsafeCell::new(false),
        intent: Cell::new(Intent::Exit),
        entry: UnsafeCell::new(Some(entry)),
        refs: UnsafeCell::new(all_refs),
        held: Cell::new(0),
        next: UnsafeCell::new(std::ptr::null()),
        prev: UnsafeCell::new(std::ptr::null()),
    });
    // The first switch "returns" into `lrt_fiber_start` with the fiber in r12 (see the layout
    // in `lrt_fiber_switch`): control words, r15 to r12, rbx, rbp, return address, and a zero
    // word where the stack is 16-byte aligned, as a call expects.
    // SAFETY: writing the initial frame inside the new stack's top 80 bytes.
    unsafe {
        let words = top as *mut u64;
        words.sub(1).write(0);
        words.sub(2).write(0);
        words.sub(3).write(lrt_fiber_start as *const () as u64);
        words.sub(4).write(0); // rbp
        words.sub(5).write(0); // rbx
        words.sub(6).write(Arc::as_ptr(&fiber) as u64); // r12
        words.sub(7).write(0); // r13
        words.sub(8).write(0); // r14
        words.sub(9).write(0); // r15
        words.sub(10).write(0x037F_0000_1F80); // MXCSR default, then the x87 control word
        *fiber.sp.get() = words.sub(10) as *mut u8;
    }
    // SAFETY: linking into the registry under the allocation lock.
    unsafe {
        GC_alloc_lock();
        let head = *REGISTRY.0.get();
        *fiber.next.get() = head;
        if !head.is_null() {
            *(*head).prev.get() = Arc::as_ptr(&fiber);
        }
        *REGISTRY.0.get() = Arc::as_ptr(&fiber);
        GC_alloc_unlock();
    }
    enqueue(fiber.clone());
    fiber
}

/// The first thing a fiber runs, on its own stack.
extern "C" fn fiber_main(fiber: *const Fiber) -> ! {
    // The worker took the allocation lock before switching here.
    // SAFETY: matched with the worker's lock.
    unsafe { GC_alloc_unlock() };
    let fiber = unsafe { &*fiber };
    // SAFETY: only this fiber takes its entry, once.
    let entry = unsafe { (*fiber.entry.get()).take() };
    if let Some(entry) = entry {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| entry(fiber)));
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

/// A reference to the running fiber that can outlive this call.
fn current_ref() -> Option<FiberRef> {
    let fiber = current()?;
    // SAFETY: the fiber is owned by an `Arc` (made in `spawn`); take another count.
    unsafe {
        Arc::increment_strong_count(fiber as *const Fiber);
        Some(Arc::from_raw(fiber as *const Fiber))
    }
}

/// Switch from the running fiber to its worker's scheduler.
#[inline(never)]
fn suspend(intent: Intent) {
    let fiber = current().expect("suspend outside a fiber");
    let worker = current_worker();
    fiber.intent.set(intent);
    // SAFETY: the worker resumes from its saved context; the lock is released on the other
    // side of the switch (see "The collector" above).
    unsafe {
        GC_alloc_lock();
        *fiber.on_cpu.get() = false;
        lrt_fiber_switch(fiber.sp.get(), *(*worker).sched_sp.get());
        GC_alloc_unlock();
    }
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

struct Worker {
    queue: Mutex<VecDeque<FiberRef>>,
    /// The scheduler's saved stack pointer while a fiber runs.
    sched_sp: UnsafeCell<*mut u8>,
    gc_thread: Cell<*mut u8>,
    native_bottom: Cell<*mut u8>,
}

// SAFETY: `sched_sp` and the cells are only used by the worker's own thread.
unsafe impl Sync for Worker {}
unsafe impl Send for Worker {}

struct Scheduler {
    workers: Vec<Worker>,
    global: Mutex<VecDeque<FiberRef>>,
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
                queue: Mutex::new(VecDeque::new()),
                sched_sp: UnsafeCell::new(std::ptr::null_mut()),
                gc_thread: Cell::new(std::ptr::null_mut()),
                native_bottom: Cell::new(std::ptr::null_mut()),
            })
            .collect(),
        global: Mutex::new(VecDeque::new()),
        idle: AtomicUsize::new(0),
        waking: AtomicBool::new(false),
    };
    if SCHEDULER.set(scheduler).is_err() {
        return;
    }
    for index in 0..count {
        let started = std::thread::Builder::new()
            .name(format!("worker-{index}"))
            .stack_size(256 * 1024)
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
    let scheduler = scheduler();
    let worker = current_worker();
    if worker.is_null() {
        lock(&scheduler.global).push_back(fiber);
    } else {
        // SAFETY: the worker lives as long as the scheduler.
        lock(unsafe { &(*worker).queue }).push_back(fiber);
    }
    wake_idle_worker();
}

/// The poller has taken a wake-up (whichever worker took it): the next one may be sent.
pub(crate) fn wake_taken() {
    if let Some(scheduler) = SCHEDULER.get() {
        scheduler.waking.store(false, Ordering::SeqCst);
    }
}

fn wake_idle_worker() {
    let scheduler = scheduler();
    if scheduler.idle.load(Ordering::SeqCst) > 0
        && scheduler.waking.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_ok()
    {
        poll::notify();
    }
}

fn worker_main(index: usize) {
    crate::tasks::register_thread();
    let scheduler = scheduler();
    let worker = &scheduler.workers[index];
    WORKER.with(|w| w.set(worker));
    let mut base = StackBase { mem_base: std::ptr::null_mut(), reg_base: std::ptr::null_mut() };
    // SAFETY: the calling thread is registered.
    let handle = unsafe { GC_get_my_stackbottom(&mut base) };
    worker.gc_thread.set(handle);
    worker.native_bottom.set(base.mem_base);
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
    !lock(&scheduler.global).is_empty() || scheduler.workers.iter().any(|w| !lock(&w.queue).is_empty())
}

fn find_work(scheduler: &Scheduler, index: usize) -> Option<FiberRef> {
    if let Some(fiber) = lock(&scheduler.workers[index].queue).pop_front() {
        return Some(fiber);
    }
    if let Some(fiber) = lock(&scheduler.global).pop_front() {
        return Some(fiber);
    }
    // Steal half of another worker's queue.
    let count = scheduler.workers.len();
    for offset in 1..count {
        let victim = &scheduler.workers[(index + offset) % count];
        let Ok(mut queue) = victim.queue.try_lock() else { continue };
        let take = queue.len().div_ceil(2);
        if take == 0 {
            continue;
        }
        let mut stolen: VecDeque<FiberRef> = queue.drain(..take).collect();
        drop(queue);
        let first = stolen.pop_front();
        if !stolen.is_empty() {
            lock(&scheduler.workers[index].queue).extend(stolen);
            wake_idle_worker();
        }
        return first;
    }
    None
}

/// Run a fiber until it switches out, then do what it asked.
fn run(worker: &Worker, fiber: FiberRef) {
    fiber.state.store(RUNNING, Ordering::SeqCst);
    CURRENT.with(|c| c.set(Arc::as_ptr(&fiber)));
    let fiber_top = StackBase { mem_base: fiber.stack_hi, reg_base: std::ptr::null_mut() };
    let native = StackBase { mem_base: worker.native_bottom.get(), reg_base: std::ptr::null_mut() };
    // SAFETY: see "The collector" above: the stack bottom changes with the stack, under the
    // allocation lock, which the fiber's side of the switch releases.
    unsafe {
        GC_alloc_lock();
        *fiber.on_cpu.get() = true;
        GC_set_stackbottom(worker.gc_thread.get(), &fiber_top);
        lrt_fiber_switch(worker.sched_sp.get(), *fiber.sp.get());
        GC_set_stackbottom(worker.gc_thread.get(), &native);
        if fiber.intent.get() == Intent::Exit {
            // Unlink from the registry while still holding the lock.
            let (prev, next) = (*fiber.prev.get(), *fiber.next.get());
            if prev.is_null() {
                *REGISTRY.0.get() = next;
            } else {
                *(*prev).next.get() = next;
            }
            if !next.is_null() {
                *(*next).prev.get() = prev;
            }
        }
        GC_alloc_unlock();
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
            fiber.state.store(DONE, Ordering::SeqCst);
            // SAFETY: the fiber has ended; nothing runs on its stack any more.
            if let Some(stack) = unsafe { (*fiber.stack.get()).take() } {
                stack.release();
            }
        }
    }
}
