//! Tasks, channels and mutexes (docs/04-runtime-and-memory.md, "Concurrency", stage 1).
//!
//! A task is an OS thread registered with the garbage collector. The objects shared between
//! tasks (a `Task`'s result slot, a `Channel`'s buffer, a `Mutex`'s value) live in collected
//! memory, which the collector scans. Their locks and condition variables are Rust's
//! futex-based `Mutex` and `Condvar`, constructed in place inside those objects; on Linux they
//! own no other memory, so the collector can free them like any other bytes.
//!
//! Everything a task needs before it is registered (its entry point's environment, and the task
//! object) is kept alive by an uncollectable two-word root until the new thread has it on its
//! stack.

use crate::{gc, out};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

#[repr(C)]
struct StackBase {
    mem_base: *mut u8,
    /// Only used on IA-64; present so the structure is never too small.
    reg_base: *mut u8,
}

#[link(name = "libgc.so.1", kind = "dylib", modifiers = "+verbatim")]
unsafe extern "C" {
    fn GC_allow_register_threads();
    fn GC_register_my_thread(base: *const StackBase) -> i32;
    fn GC_unregister_my_thread() -> i32;
    fn GC_get_stack_base(base: *mut StackBase) -> i32;
    fn GC_malloc_uncollectable(size: usize) -> *mut u8;
    fn GC_free(ptr: *mut u8);
}

/// Let other threads register with the collector. This puts the collector into its
/// multi-threaded mode, which makes every allocation about half again as slow, so it happens
/// on the first `spawn` rather than at startup. The first `spawn` is always on the main thread,
/// since no other thread exists before it, which is where the collector requires the call.
fn allow_threads() {
    static ALLOWED: std::sync::Once = std::sync::Once::new();
    // SAFETY: called on the main thread (see above), after `GC_init`.
    ALLOWED.call_once(|| unsafe { GC_allow_register_threads() });
}

// ---- the cap on live tasks ----------------------------------------------------------------

static LIVE: Mutex<i64> = Mutex::new(0);
static SLOT_FREED: Condvar = Condvar::new();

fn max_tasks() -> i64 {
    static MAX: AtomicI64 = AtomicI64::new(0);
    let cached = MAX.load(Ordering::Relaxed);
    if cached > 0 {
        return cached;
    }
    let max = std::env::var(lang_names::env_var("MAX_TASKS")).ok().and_then(|v| v.parse().ok()).unwrap_or(10_000);
    MAX.store(max, Ordering::Relaxed);
    max
}

fn stack_size() -> usize {
    std::env::var(lang_names::env_var("TASK_STACK")).ok().and_then(|v| v.parse().ok()).unwrap_or(8 * 1024 * 1024)
}

// ---- tasks --------------------------------------------------------------------------------

#[repr(C)]
pub struct Task {
    done: Mutex<bool>,
    finished: Condvar,
    /// Collected memory for the result, or null for a task whose function returns nothing.
    result: *mut u8,
}

/// A new task object with room for a result of `result_size` bytes.
#[unsafe(no_mangle)]
pub extern "C" fn lrt_task_new(result_size: i64) -> *mut Task {
    let task = gc::alloc(std::mem::size_of::<Task>()) as *mut Task;
    let result = if result_size > 0 { gc::alloc(result_size as usize) } else { std::ptr::null_mut() };
    // SAFETY: `task` is a fresh, suitably aligned allocation of the right size.
    unsafe { task.write(Task { done: Mutex::new(false), finished: Condvar::new(), result }) };
    task
}

/// Where the task's function stores its result.
///
/// # Safety
/// `task` comes from `lrt_task_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lrt_task_result(task: *mut Task) -> *mut u8 {
    unsafe { (*task).result }
}

struct SendPtr(*mut *mut u8);
// SAFETY: the pointer is to an uncollectable root that only the new thread reads and frees.
unsafe impl Send for SendPtr {}

/// Start a thread running `entry(env)`, then marking `task` done. Waits first if the cap on
/// live tasks is reached.
///
/// # Safety
/// `entry` must be a function generated for this `spawn`, `env` its environment and `task` the
/// task it reports to.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lrt_spawn(entry: extern "C" fn(*mut u8), env: *mut u8, task: *mut Task) {
    allow_threads();
    {
        let mut live = LIVE.lock().unwrap_or_else(|p| p.into_inner());
        while *live >= max_tasks() {
            live = SLOT_FREED.wait(live).unwrap_or_else(|p| p.into_inner());
        }
        *live += 1;
    }
    // SAFETY: a plain allocation; the collector scans it as a root until it is freed.
    let roots = unsafe { GC_malloc_uncollectable(2 * std::mem::size_of::<*mut u8>()) as *mut *mut u8 };
    unsafe {
        roots.write(env);
        roots.add(1).write(task as *mut u8);
    }
    let roots = SendPtr(roots);
    let started = std::thread::Builder::new().stack_size(stack_size()).spawn(move || {
        let roots = roots;
        let mut base = StackBase { mem_base: std::ptr::null_mut(), reg_base: std::ptr::null_mut() };
        // SAFETY: registering the current thread, with its own stack base.
        unsafe {
            GC_get_stack_base(&mut base);
            GC_register_my_thread(&base);
        }
        // From here the collector scans this thread's stack, so the roots can go.
        let (env, task) = unsafe { (roots.0.read(), roots.0.add(1).read() as *mut Task) };
        unsafe { GC_free(roots.0 as *mut u8) };
        entry(env);
        // SAFETY: `task` stays alive: it is on this stack.
        unsafe {
            let mut done = (*task).done.lock().unwrap_or_else(|p| p.into_inner());
            *done = true;
            (*task).finished.notify_all();
        }
        {
            let mut live = LIVE.lock().unwrap_or_else(|p| p.into_inner());
            *live -= 1;
            SLOT_FREED.notify_one();
        }
        // SAFETY: the thread registered itself above.
        unsafe { GC_unregister_my_thread() };
    });
    if let Err(error) = started {
        out::flush();
        eprintln!("fatal: could not start a task: {error}");
        std::process::abort();
    }
}

/// Wait until the task has finished.
///
/// # Safety
/// `task` comes from `lrt_task_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lrt_task_join(task: *mut Task) {
    let task = unsafe { &*task };
    let mut done = task.done.lock().unwrap_or_else(|p| p.into_inner());
    while !*done {
        done = task.finished.wait(done).unwrap_or_else(|p| p.into_inner());
    }
}

/// Wait up to `seconds`; returns 1 if the task has finished.
///
/// # Safety
/// `task` comes from `lrt_task_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lrt_task_join_timeout(task: *mut Task, seconds: f64) -> i64 {
    let task = unsafe { &*task };
    let limit = Duration::from_secs_f64(seconds.max(0.0));
    let done = task.done.lock().unwrap_or_else(|p| p.into_inner());
    let (done, _) = task.finished.wait_timeout_while(done, limit, |done| !*done).unwrap_or_else(|p| p.into_inner());
    i64::from(*done)
}

/// # Safety
/// `task` comes from `lrt_task_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lrt_task_is_done(task: *mut Task) -> i64 {
    let task = unsafe { &*task };
    i64::from(*task.done.lock().unwrap_or_else(|p| p.into_inner()))
}

// ---- channels -----------------------------------------------------------------------------

#[repr(C)]
pub struct Channel {
    lock: Mutex<()>,
    not_empty: Condvar,
    not_full: Condvar,
    /// Collected memory for `slots` elements.
    buffer: *mut u8,
    elem_size: usize,
    /// Room in the buffer: the capacity, or 1 for a capacity of 0.
    slots: usize,
    /// 0 means a send waits until a receiver has taken the value.
    capacity: i64,
    head: usize,
    len: usize,
    closed: bool,
    /// Values sent and received so far, for capacity-0 channels.
    sent: u64,
    received: u64,
}

/// A channel of elements `elem_size` bytes long.
#[unsafe(no_mangle)]
pub extern "C" fn lrt_channel_new(elem_size: i64, capacity: i64) -> *mut Channel {
    let slots = capacity.max(1) as usize;
    let elem_size = elem_size.max(0) as usize;
    let channel = gc::alloc(std::mem::size_of::<Channel>()) as *mut Channel;
    let buffer = gc::alloc((slots * elem_size).max(1));
    // SAFETY: a fresh, suitably aligned allocation of the right size.
    unsafe {
        channel.write(Channel {
            lock: Mutex::new(()),
            not_empty: Condvar::new(),
            not_full: Condvar::new(),
            buffer,
            elem_size,
            slots,
            capacity,
            head: 0,
            len: 0,
            closed: false,
            sent: 0,
            received: 0,
        })
    };
    channel
}

/// Send the value at `value`, waiting while the channel is full. Panics if it is closed.
///
/// # Safety
/// `channel` comes from `lrt_channel_new` and `value` points to one element.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lrt_channel_send(channel: *mut Channel, value: *const u8, loc: *const u8, loc_len: i64) {
    // The channel's fields are only read or written while holding its lock.
    let state = channel;
    let mut guard = unsafe { (*state).lock.lock().unwrap_or_else(|p| p.into_inner()) };
    loop {
        let c = unsafe { &*state };
        if c.closed {
            drop(guard);
            unsafe { crate::panic::lrt_panic(b"send on a closed channel".as_ptr(), 24, loc, loc_len) };
        }
        if c.len < c.slots {
            break;
        }
        guard = c.not_full.wait(guard).unwrap_or_else(|p| p.into_inner());
    }
    let c = unsafe { &mut *state };
    let at = (c.head + c.len) % c.slots;
    unsafe { std::ptr::copy_nonoverlapping(value, c.buffer.add(at * c.elem_size), c.elem_size) };
    c.len += 1;
    c.sent += 1;
    let ticket = c.sent;
    c.not_empty.notify_one();
    if c.capacity == 0 {
        // A rendezvous: wait until a receiver has taken this value.
        loop {
            let (received, closed) = unsafe { ((*state).received, (*state).closed) };
            if received >= ticket || closed {
                break;
            }
            guard = unsafe { (*state).not_full.wait(guard).unwrap_or_else(|p| p.into_inner()) };
        }
    }
    drop(guard);
}

/// Receive into `out`. With `wait`, waits for a value; returns 1 if one was received and 0 if
/// the channel is closed and empty (or, without `wait`, if nothing is ready).
///
/// # Safety
/// `channel` comes from `lrt_channel_new` and `out` has room for one element.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lrt_channel_recv(channel: *mut Channel, out: *mut u8, wait: i64) -> i64 {
    let state = channel;
    let mut guard = unsafe { (*state).lock.lock().unwrap_or_else(|p| p.into_inner()) };
    loop {
        let c = unsafe { &*state };
        if c.len > 0 {
            break;
        }
        if c.closed || wait == 0 {
            return 0;
        }
        guard = c.not_empty.wait(guard).unwrap_or_else(|p| p.into_inner());
    }
    let c = unsafe { &mut *state };
    let slot = unsafe { c.buffer.add(c.head * c.elem_size) };
    unsafe {
        std::ptr::copy_nonoverlapping(slot, out, c.elem_size);
        // Clear the slot so the collector does not keep what it referred to alive.
        std::ptr::write_bytes(slot, 0, c.elem_size);
    }
    c.head = (c.head + 1) % c.slots;
    c.len -= 1;
    c.received += 1;
    c.not_full.notify_all();
    drop(guard);
    1
}

/// Close the channel: receivers drain what is left, then get `None`; sending panics.
///
/// # Safety
/// `channel` comes from `lrt_channel_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lrt_channel_close(channel: *mut Channel) {
    let guard = unsafe { (*channel).lock.lock().unwrap_or_else(|p| p.into_inner()) };
    unsafe {
        (*channel).closed = true;
        (*channel).not_empty.notify_all();
        (*channel).not_full.notify_all();
    }
    drop(guard);
}

// ---- mutexes ------------------------------------------------------------------------------

#[repr(C)]
pub struct ValueMutex {
    locked: Mutex<bool>,
    released: Condvar,
    /// Collected memory holding the protected value.
    value: *mut u8,
}

/// A mutex over a value of `size` bytes; the caller stores the value at `lrt_mutex_value`.
#[unsafe(no_mangle)]
pub extern "C" fn lrt_mutex_new(size: i64) -> *mut ValueMutex {
    let mutex = gc::alloc(std::mem::size_of::<ValueMutex>()) as *mut ValueMutex;
    let value = gc::alloc((size.max(1)) as usize);
    // SAFETY: a fresh, suitably aligned allocation of the right size.
    unsafe { mutex.write(ValueMutex { locked: Mutex::new(false), released: Condvar::new(), value }) };
    mutex
}

/// Where the protected value is.
///
/// # Safety
/// `mutex` comes from `lrt_mutex_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lrt_mutex_value(mutex: *mut ValueMutex) -> *mut u8 {
    unsafe { (*mutex).value }
}

/// Wait for the lock and take it. Not re-entrant: a task that locks a mutex it holds waits
/// forever, as in Go.
///
/// # Safety
/// `mutex` comes from `lrt_mutex_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lrt_mutex_lock(mutex: *mut ValueMutex) -> *mut ValueMutex {
    let m = unsafe { &*mutex };
    let mut locked = m.locked.lock().unwrap_or_else(|p| p.into_inner());
    while *locked {
        locked = m.released.wait(locked).unwrap_or_else(|p| p.into_inner());
    }
    *locked = true;
    mutex
}

/// # Safety
/// `mutex` comes from `lrt_mutex_new` and is locked.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lrt_mutex_unlock(mutex: *mut ValueMutex) {
    let m = unsafe { &*mutex };
    let mut locked = m.locked.lock().unwrap_or_else(|p| p.into_inner());
    *locked = false;
    m.released.notify_one();
}

// ---- time ---------------------------------------------------------------------------------

/// Pause the calling task.
#[unsafe(no_mangle)]
pub extern "C" fn lrt_time_sleep(seconds: f64) {
    if seconds > 0.0 {
        std::thread::sleep(Duration::from_secs_f64(seconds));
    }
}

/// Seconds since an arbitrary fixed point, never going backwards.
#[unsafe(no_mangle)]
pub extern "C" fn lrt_time_monotonic() -> f64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_secs_f64()
}

/// Seconds since the Unix epoch.
#[unsafe(no_mangle)]
pub extern "C" fn lrt_time_now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}
