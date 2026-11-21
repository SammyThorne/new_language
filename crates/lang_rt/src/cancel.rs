//! Deadlines and cancellation (docs/14, milestone 13).
//!
//! Every task's context (`log::Context`) carries a deadline and a cancel reason. A route with a
//! `timeout` gives the task serving the request a deadline; a client that hangs up while its
//! handler runs cancels that task. Waiting operations look at the reason before they wait (one
//! load) and fail with a [`Cancelled`] error: socket reads and writes (so client calls and
//! `postgres` queries), channel receives and the database pool's wait. `sleep` returns early.
//! Code that never waits is not interrupted; `http.check_cancelled()` is there for long loops.
//!
//! A task with a deadline is kept in [`WATCHED`], which the timer thread sweeps with the I/O
//! deadlines every quarter second: a task whose deadline has passed is marked and woken from
//! whatever it waits on. Tasks spawned by a task with a deadline inherit it, and are watched
//! until they end; they are not cancelled when the client hangs up.

use crate::fiber::{self, Fiber, FiberRef, lock};
use crate::poll;
use std::cell::Cell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

const NONE: u8 = 0;
const DEADLINE: u8 = 1;
const CLIENT_CLOSED: u8 = 2;
const NOT_WATCHED: usize = usize::MAX;

/// Why a task was cancelled.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Reason {
    /// The request's deadline (the route's `timeout`) passed.
    Deadline,
    /// The client closed the connection while the handler ran.
    ClientClosed,
}

impl Reason {
    fn from_code(code: u8) -> Option<Reason> {
        match code {
            DEADLINE => Some(Reason::Deadline),
            CLIENT_CLOSED => Some(Reason::ClientClosed),
            _ => None,
        }
    }

    fn code(self) -> u8 {
        match self {
            Reason::Deadline => DEADLINE,
            Reason::ClientClosed => CLIENT_CLOSED,
        }
    }

    pub(crate) fn message(self) -> &'static str {
        match self {
            Reason::Deadline => "cancelled: the request's deadline passed",
            Reason::ClientClosed => "cancelled: the client closed the connection",
        }
    }

    /// The error a cancelled socket operation returns.
    pub(crate) fn io_error(self) -> std::io::Error {
        std::io::Error::other(Cancelled(self))
    }
}

/// The error of an operation that a cancellation stopped.
#[derive(Debug)]
pub(crate) struct Cancelled(pub(crate) Reason);

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0.message())
    }
}

impl std::error::Error for Cancelled {}

/// Whether a socket error is a cancellation, and which.
pub(crate) fn reason_of(error: &std::io::Error) -> Option<Reason> {
    error.get_ref().and_then(|e| e.downcast_ref::<Cancelled>()).map(|c| c.0)
}

/// A task's deadline and cancellation, kept in its context.
pub(crate) struct State {
    /// On the [`poll::coarse_now`] clock; 0 for none. Only the task itself touches it.
    deadline: Cell<u64>,
    /// [`NONE`], or why the task was cancelled. Set by the sweep and the poller too.
    reason: AtomicU8,
    /// A handler runs on this task's connection, so a hang-up cancels it.
    watching: AtomicBool,
    /// The task's place in [`WATCHED`], or [`NOT_WATCHED`].
    slot: Cell<usize>,
}

impl State {
    pub(crate) const fn new() -> State {
        State {
            deadline: Cell::new(0),
            reason: AtomicU8::new(NONE),
            watching: AtomicBool::new(false),
            slot: Cell::new(NOT_WATCHED),
        }
    }

    /// A spawned task's state: its parent's deadline, and nothing else.
    pub(crate) fn inherited(&self) -> State {
        let state = State::new();
        state.deadline.set(self.deadline.get());
        state
    }

    pub(crate) fn has_deadline(&self) -> bool {
        self.deadline.get() != 0
    }

    /// Mark the task cancelled, unless it already is. Returns whether this call did it.
    fn cancel(&self, reason: Reason) -> bool {
        self.reason.compare_exchange(NONE, reason.code(), Ordering::SeqCst, Ordering::SeqCst).is_ok()
    }
}

/// Tasks with a deadline, for the sweep: each with its deadline.
static WATCHED: Mutex<Watched> = Mutex::new(Watched { slots: Vec::new(), free: Vec::new() });

struct Watched {
    slots: Vec<Option<(u64, FiberRef)>>,
    free: Vec<usize>,
}

fn state(fiber: &Fiber) -> &State {
    &fiber.context.cancel
}

/// Put a task with a deadline where the sweep sees it, or update its deadline there.
fn watch(fiber: FiberRef, deadline: u64) {
    let state = state(&fiber);
    let mut watched = lock(&WATCHED);
    let slot = state.slot.get();
    if slot != NOT_WATCHED {
        if let Some(entry) = watched.slots[slot].as_mut() {
            entry.0 = deadline;
        }
        return;
    }
    let slot = match watched.free.pop() {
        Some(slot) => slot,
        None => {
            watched.slots.push(None);
            watched.slots.len() - 1
        }
    };
    state.slot.set(slot);
    watched.slots[slot] = Some((deadline, fiber));
}

fn unwatch(fiber: &Fiber) {
    let state = state(fiber);
    let slot = state.slot.replace(NOT_WATCHED);
    if slot == NOT_WATCHED {
        return;
    }
    let entry = {
        let mut watched = lock(&WATCHED);
        watched.free.push(slot);
        watched.slots[slot].take()
    };
    // The reference is dropped outside the lock.
    drop(entry);
}

/// A new task inherited a deadline: watch it until it ends.
pub(crate) fn spawned(fiber: &FiberRef) {
    let deadline = state(fiber).deadline.get();
    if deadline != 0 {
        watch(fiber.clone(), deadline);
    }
}

/// A task has ended.
pub(crate) fn ended(fiber: &Fiber) {
    if state(fiber).slot.get() != NOT_WATCHED {
        unwatch(fiber);
    }
}

/// Wake the tasks whose deadline has passed (from the timer thread's sweep).
pub(crate) fn sweep(now: u64) {
    let mut due = Vec::new();
    {
        let watched = lock(&WATCHED);
        for (deadline, fiber) in watched.slots.iter().flatten() {
            if *deadline <= now && state(fiber).cancel(Reason::Deadline) {
                due.push(fiber.clone());
            }
        }
    }
    for fiber in due {
        fiber::unpark(&fiber);
    }
}

/// Whether the running task has been cancelled, and why: a load, and a look at the clock only
/// for a task with a deadline. Outside a task, never.
#[inline]
pub(crate) fn check() -> Option<Reason> {
    let fiber = fiber::current()?;
    let state = state(fiber);
    let reason = state.reason.load(Ordering::Relaxed);
    if reason != NONE {
        return Reason::from_code(reason);
    }
    let deadline = state.deadline.get();
    if deadline != 0 && poll::coarse_now() >= deadline {
        state.cancel(Reason::Deadline);
        return Reason::from_code(state.reason.load(Ordering::SeqCst));
    }
    None
}

/// Give the running task a deadline `nanos` from now (a route's `timeout`).
pub(crate) fn set_timeout(nanos: u64) {
    let Some(fiber) = fiber::current_ref() else { return };
    let deadline = poll::coarse_now().saturating_add(nanos).max(1);
    state(&fiber).deadline.set(deadline);
    watch(fiber, deadline);
}

/// A handler is about to run on the running task's connection: from now a hang-up cancels it.
/// `hung_up` says whether the client has hung up already.
#[inline]
pub(crate) fn begin_request(hung_up: bool) {
    if let Some(fiber) = fiber::current() {
        let state = state(fiber);
        state.watching.store(true, Ordering::SeqCst);
        if hung_up {
            state.cancel(Reason::ClientClosed);
        }
    }
}

/// The handler has returned: forget the deadline and stop watching for a hang-up. Returns why
/// the handler was cancelled, if it was.
#[inline]
pub(crate) fn end_request() -> Option<Reason> {
    let fiber = fiber::current()?;
    let state = state(fiber);
    if state.slot.get() != NOT_WATCHED {
        unwatch(fiber);
    }
    state.deadline.set(0);
    state.watching.store(false, Ordering::SeqCst);
    Reason::from_code(state.reason.swap(NONE, Ordering::SeqCst))
}

/// The client of a connection served by `fiber` hung up (from the poller): cancel the handler,
/// if one runs, and wake it from whatever it waits on.
pub(crate) fn hung_up(fiber: &FiberRef) {
    let state = state(fiber);
    if state.watching.load(Ordering::SeqCst) && state.cancel(Reason::ClientClosed) {
        fiber::unpark(fiber);
    }
}

// ---- for compiled code ------------------------------------------------------------------

/// 0 if the running task goes on, 1 if its deadline has passed, 2 if its client hung up.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_cancel_state() -> i64 {
    check().map_or(0, |reason| i64::from(reason.code()))
}

/// The running task's deadline in nanoseconds since 1970, or -1 if it has none.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_cancel_deadline() -> i64 {
    let Some(fiber) = fiber::current() else { return -1 };
    let deadline = state(fiber).deadline.get();
    if deadline == 0 {
        return -1;
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let left = deadline as i128 - poll::coarse_now() as i128;
    (now.as_nanos() as i128 + left).clamp(0, i128::from(i64::MAX)) as i64
}
