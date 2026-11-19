//! Tasks, channels and mutexes (docs/04-runtime-and-memory.md, "Concurrency", stage 2).
//!
//! A task is a fiber (`fiber.rs`). The objects shared between tasks (a `Task`'s result slot, a
//! `Channel`'s buffer, a `Mutex`'s value) live in collected memory, which the collector scans.
//! Each holds a Rust futex-based `Mutex`, constructed in place, guarding its state and its
//! [`WaitList`]s; the lock owns no other memory and is never held while waiting, and the waiters
//! are nodes on the waiting tasks' own stacks, so the collector can free these objects like any
//! other bytes. Waiting parks the fiber, or blocks the thread when called from the main thread.

use crate::fiber::{self, WaitList, WaitNode};
use crate::gc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

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
    fn GC_get_stack_base(base: *mut StackBase) -> i32;
    fn GC_malloc_uncollectable(size: usize) -> *mut u8;
    fn GC_free(ptr: *mut u8);
}

/// Let other threads register with the collector. This puts the collector into its
/// multi-threaded mode, which makes every allocation about half again as slow, so it happens
/// on the first `spawn` rather than at startup. The first `spawn` is always on the main thread,
/// since no other thread exists before it, which is where the collector requires the call.
pub(crate) fn allow_threads() {
    static ALLOWED: std::sync::Once = std::sync::Once::new();
    // SAFETY: called on the main thread (see above), after `GC_init`.
    ALLOWED.call_once(|| unsafe { GC_allow_register_threads() });
}

/// Register the calling thread with the collector, for a thread the runtime started.
pub(crate) fn register_thread() {
    let mut base = StackBase { mem_base: std::ptr::null_mut(), reg_base: std::ptr::null_mut() };
    // SAFETY: registering the current thread, with its own stack base.
    unsafe {
        GC_get_stack_base(&mut base);
        GC_register_my_thread(&base);
    }
}

/// Two words the collector treats as a root until [`free_roots`]: for handing collected
/// pointers to a thread that is not registered yet, or keeping them alive while Rust holds them.
pub(crate) fn roots(a: *mut u8, b: *mut u8) -> *mut *mut u8 {
    // SAFETY: a plain allocation; the collector scans it until it is freed.
    let roots = unsafe { GC_malloc_uncollectable(2 * std::mem::size_of::<*mut u8>()) as *mut *mut u8 };
    unsafe {
        roots.write(a);
        roots.add(1).write(b);
    }
    roots
}

pub(crate) fn free_roots(roots: *mut *mut u8) {
    // SAFETY: allocated by `roots`.
    unsafe { GC_free(roots as *mut u8) };
}

// ---- tasks --------------------------------------------------------------------------------

/// The state shared by a task and those who join it. In collected memory; the lock is Rust's
/// futex-based `Mutex`, which owns no other memory, and the waiters live on their own stacks.
#[repr(C)]
pub struct Task {
    state: Mutex<TaskState>,
    /// Collected memory for the result, or null for a task whose function returns nothing.
    result: *mut u8,
}

struct TaskState {
    done: bool,
    panicked: bool,
    joiners: WaitList,
}

/// A new task object with room for a result of `result_size` bytes.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_task_new(result_size: i64) -> *mut Task {
    let task = gc::alloc(std::mem::size_of::<Task>()) as *mut Task;
    let result = if result_size > 0 { gc::alloc(result_size as usize) } else { std::ptr::null_mut() };
    let state = TaskState { done: false, panicked: false, joiners: WaitList::new() };
    // SAFETY: `task` is a fresh, suitably aligned allocation of the right size.
    unsafe { task.write(Task { state: Mutex::new(state), result }) };
    task
}

/// Where the task's function stores its result.
///
/// # Safety
/// `task` comes from `lrt_task_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_task_result(task: *mut Task) -> *mut u8 {
    unsafe { (*task).result }
}

/// Start a fiber running `entry(env)`, then marking `task` done.
///
/// # Safety
/// `entry` must be a function generated for this `spawn`, `env` its environment and `task` the
/// task it reports to.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_spawn(entry: extern "C-unwind" fn(*mut u8), env: *mut u8, task: *mut Task) {
    fiber::spawn([env, task as *mut u8], fiber::Entry::Plain(run_task, entry as usize));
}

/// The body of a task's fiber: run `entry`, then mark the task done.
fn run_task(me: &fiber::Fiber, entry: usize) {
    // SAFETY: the function pointer `lrt_spawn` was given.
    let entry: extern "C-unwind" fn(*mut u8) = unsafe { std::mem::transmute(entry) };
    // The environment and task are read from the fiber, which the collector scans.
    let (env, task) = me.refs();
    let task = task as *mut Task;
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| entry(env)));
    if outcome.is_err() {
        fiber::release_held(me);
    }
    // SAFETY: `task` is kept alive by the fiber's references.
    let mut state = fiber::lock(unsafe { &(*task).state });
    state.done = true;
    state.panicked = outcome.is_err();
    state.joiners.wake_all();
}

/// Wait until the task has finished, or until `deadline`. Returns whether it has finished.
fn wait_for(task: &Task, deadline: Option<Instant>) -> bool {
    if fiber::lock(&task.state).done {
        return true;
    }
    loop {
        let node = WaitNode::new();
        {
            let mut state = fiber::lock(&task.state);
            if state.done {
                return true;
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return false;
            }
            state.joiners.push(&node);
        }
        while !node.signaled() {
            if deadline.is_some_and(|d| Instant::now() >= d) {
                let mut state = fiber::lock(&task.state);
                state.joiners.remove(&node);
                return state.done;
            }
            fiber::park(deadline);
        }
    }
}

/// Wait until the task has finished. Joining a task that panicked panics.
///
/// # Safety
/// `task` comes from `lrt_task_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_task_join(task: *mut Task) {
    let task = unsafe { &*task };
    wait_for(task, None);
    if fiber::lock(&task.state).panicked {
        crate::panic::panic_at("joined a task that panicked", "");
    }
}

/// Wait up to `seconds`; returns 1 if the task has finished.
///
/// # Safety
/// `task` comes from `lrt_task_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_task_join_timeout(task: *mut Task, seconds: f64) -> i64 {
    let task = unsafe { &*task };
    let deadline = Instant::now() + Duration::from_secs_f64(seconds.clamp(0.0, 1e9));
    i64::from(wait_for(task, Some(deadline)))
}

/// Wait until the task has finished; 1 if it panicked. For the test runner.
///
/// # Safety
/// `task` comes from `lrt_task_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_task_panicked(task: *mut Task) -> i64 {
    let task = unsafe { &*task };
    wait_for(task, None);
    i64::from(fiber::lock(&task.state).panicked)
}

/// # Safety
/// `task` comes from `lrt_task_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_task_is_done(task: *mut Task) -> i64 {
    let task = unsafe { &*task };
    i64::from(fiber::lock(&task.state).done)
}

// ---- channels -----------------------------------------------------------------------------

#[repr(C)]
pub struct Channel {
    state: Mutex<ChannelState>,
    /// Collected memory for `slots` elements.
    buffer: *mut u8,
    elem_size: usize,
    /// Room in the buffer: the capacity, or 1 for a capacity of 0.
    slots: usize,
    /// 0 means a send waits until a receiver has taken the value.
    capacity: i64,
}

struct ChannelState {
    head: usize,
    len: usize,
    closed: bool,
    /// Values sent and received so far, for capacity-0 channels.
    sent: u64,
    received: u64,
    receivers: WaitList,
    /// Senders waiting for room, or for their value to be taken.
    senders: WaitList,
}

/// A channel of elements `elem_size` bytes long.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_channel_new(elem_size: i64, capacity: i64) -> *mut Channel {
    let slots = capacity.max(1) as usize;
    let elem_size = elem_size.max(0) as usize;
    let channel = gc::alloc(std::mem::size_of::<Channel>()) as *mut Channel;
    let buffer = gc::alloc((slots * elem_size).max(1));
    let state = ChannelState {
        head: 0,
        len: 0,
        closed: false,
        sent: 0,
        received: 0,
        receivers: WaitList::new(),
        senders: WaitList::new(),
    };
    // SAFETY: a fresh, suitably aligned allocation of the right size.
    unsafe { channel.write(Channel { state: Mutex::new(state), buffer, elem_size, slots, capacity }) };
    channel
}

/// Send the value at `value`, waiting while the channel is full. Panics if it is closed.
///
/// # Safety
/// `channel` comes from `lrt_channel_new` and `value` points to one element.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_channel_send(
    channel: *mut Channel,
    value: *const u8,
    loc: *const u8,
    loc_len: i64,
) {
    let c = unsafe { &*channel };
    let ticket = loop {
        let node = WaitNode::new();
        {
            let mut state = fiber::lock(&c.state);
            if state.closed {
                drop(state);
                unsafe { crate::panic::lrt_panic(b"send on a closed channel".as_ptr(), 24, loc, loc_len) };
            }
            if state.len < c.slots {
                let at = (state.head + state.len) % c.slots;
                unsafe { std::ptr::copy_nonoverlapping(value, c.buffer.add(at * c.elem_size), c.elem_size) };
                state.len += 1;
                state.sent += 1;
                state.receivers.wake_one();
                break state.sent;
            }
            state.senders.push(&node);
        }
        node.wait();
    };
    if c.capacity == 0 {
        // A rendezvous: wait until a receiver has taken this value.
        loop {
            let node = WaitNode::new();
            {
                let mut state = fiber::lock(&c.state);
                if state.received >= ticket || state.closed {
                    return;
                }
                state.senders.push(&node);
            }
            node.wait();
        }
    }
}

/// Receive into `out`. With `wait`, waits for a value; returns 1 if one was received and 0 if
/// the channel is closed and empty (or, without `wait`, if nothing is ready).
///
/// # Safety
/// `channel` comes from `lrt_channel_new` and `out` has room for one element.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_channel_recv(channel: *mut Channel, out: *mut u8, wait: i64) -> i64 {
    let c = unsafe { &*channel };
    loop {
        let node = WaitNode::new();
        {
            let mut state = fiber::lock(&c.state);
            if state.len > 0 {
                let slot = unsafe { c.buffer.add(state.head * c.elem_size) };
                unsafe {
                    std::ptr::copy_nonoverlapping(slot, out, c.elem_size);
                    // Clear the slot so the collector does not keep what it referred to alive.
                    std::ptr::write_bytes(slot, 0, c.elem_size);
                }
                state.head = (state.head + 1) % c.slots;
                state.len -= 1;
                state.received += 1;
                state.senders.wake_all();
                return 1;
            }
            if state.closed || wait == 0 {
                return 0;
            }
            state.receivers.push(&node);
        }
        node.wait();
    }
}

/// Close the channel: receivers drain what is left, then get `None`; sending panics.
///
/// # Safety
/// `channel` comes from `lrt_channel_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_channel_close(channel: *mut Channel) {
    let mut state = fiber::lock(unsafe { &(*channel).state });
    state.closed = true;
    state.receivers.wake_all();
    state.senders.wake_all();
}

// ---- mutexes ------------------------------------------------------------------------------

#[repr(C)]
pub struct ValueMutex {
    state: Mutex<MutexState>,
    /// Collected memory holding the protected value.
    value: *mut u8,
}

struct MutexState {
    locked: bool,
    waiters: WaitList,
}

/// A mutex over a value of `size` bytes; the caller stores the value at `lrt_mutex_value`.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_mutex_new(size: i64) -> *mut ValueMutex {
    let mutex = gc::alloc(std::mem::size_of::<ValueMutex>()) as *mut ValueMutex;
    let value = gc::alloc((size.max(1)) as usize);
    let state = MutexState { locked: false, waiters: WaitList::new() };
    // SAFETY: a fresh, suitably aligned allocation of the right size.
    unsafe { mutex.write(ValueMutex { state: Mutex::new(state), value }) };
    mutex
}

/// Where the protected value is.
///
/// # Safety
/// `mutex` comes from `lrt_mutex_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_mutex_value(mutex: *mut ValueMutex) -> *mut u8 {
    unsafe { (*mutex).value }
}

/// Wait for the lock and take it. Not re-entrant: a task that locks a mutex it holds waits
/// forever, as in Go.
///
/// # Safety
/// `mutex` comes from `lrt_mutex_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_mutex_lock(mutex: *mut ValueMutex) -> *mut ValueMutex {
    let m = unsafe { &*mutex };
    loop {
        let node = WaitNode::new();
        {
            let mut state = fiber::lock(&m.state);
            if !state.locked {
                state.locked = true;
                if let Some(me) = fiber::current() {
                    me.hold(mutex as *mut u8);
                }
                return mutex;
            }
            state.waiters.push(&node);
        }
        node.wait();
    }
}

/// # Safety
/// `mutex` comes from `lrt_mutex_new` and is locked.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_mutex_unlock(mutex: *mut ValueMutex) {
    if let Some(me) = fiber::current() {
        me.release(mutex as *mut u8);
    }
    unlock_for_panic(mutex as *mut u8);
}

/// Release a mutex (also for a task that panicked while holding it).
pub(crate) fn unlock_for_panic(mutex: *mut u8) {
    let m = unsafe { &*(mutex as *const ValueMutex) };
    let mut state = fiber::lock(&m.state);
    state.locked = false;
    state.waiters.wake_one();
}

// ---- time ---------------------------------------------------------------------------------

/// Pause the calling task.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_time_sleep(seconds: f64) {
    if seconds <= 0.0 {
        return;
    }
    let deadline = Instant::now() + Duration::from_secs_f64(seconds.min(1e9));
    if fiber::current().is_none() {
        std::thread::sleep(deadline - Instant::now());
        return;
    }
    while Instant::now() < deadline {
        fiber::park(Some(deadline));
    }
}

/// Seconds since an arbitrary fixed point, never going backwards.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_time_monotonic() -> f64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_secs_f64()
}

/// Seconds since the Unix epoch.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_time_now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}
