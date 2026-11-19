//! The network poller and timers behind fibers (docs/04-runtime-and-memory.md, "Stage 2").
//!
//! Sockets are non-blocking and registered once with a shared epoll instance, edge-triggered.
//! A fiber whose read or write would block records itself as the waiter of its [`IoSource`] and
//! parks; whichever worker next calls [`poll`] (idle workers wait there; busy ones look now and
//! then) marks the source ready and wakes the waiter. An eventfd in the same epoll set lets
//! other threads wake an idle worker when work arrives.
//!
//! Sources are never freed, only recycled, so an event that arrives for a source that has since
//! been closed and reused only causes a spurious wake-up, which every waiter tolerates.
//!
//! One timer thread wakes fibers parked with a deadline (`sleep`, `join` with a timeout), and
//! every quarter second wakes I/O waiters whose deadline has passed (idle and request timeouts,
//! WebSocket keep-alives). Deadlines on I/O are swept rather than put in the timer heap, since
//! almost all of them are cancelled by the I/O arriving first.

use crate::fiber::{self, FiberRef, Waker, lock};
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct EpollEvent {
    events: u32,
    data: u64,
}

const EPOLLIN: u32 = 0x001;
const EPOLLOUT: u32 = 0x004;
const EPOLLERR: u32 = 0x008;
const EPOLLHUP: u32 = 0x010;
const EPOLLRDHUP: u32 = 0x2000;
const EPOLLET: u32 = 1 << 31;
const EPOLL_CTL_ADD: i32 = 1;
const EPOLL_CTL_DEL: i32 = 2;
const EPOLL_CLOEXEC: i32 = 0o2000000;
const EFD_NONBLOCK: i32 = 0o4000;
const EFD_CLOEXEC: i32 = 0o2000000;
const POLLIN: i16 = 1;
const POLLOUT: i16 = 4;

unsafe extern "C" {
    fn epoll_create1(flags: i32) -> i32;
    fn epoll_ctl(epfd: i32, op: i32, fd: i32, event: *mut EpollEvent) -> i32;
    fn epoll_wait(epfd: i32, events: *mut EpollEvent, max: i32, timeout: i32) -> i32;
    fn eventfd(initval: u32, flags: i32) -> i32;
    fn read(fd: i32, buf: *mut std::ffi::c_void, count: usize) -> isize;
    fn write(fd: i32, buf: *const std::ffi::c_void, count: usize) -> isize;
    #[link_name = "poll"]
    fn sys_poll(fds: *mut PollFd, count: u64, timeout: i32) -> i32;
    fn clock_gettime(clock: i32, time: *mut Timespec) -> i32;
}

#[repr(C)]
struct Timespec {
    seconds: i64,
    nanoseconds: i64,
}

const CLOCK_REALTIME_COARSE: i32 = 5;
const CLOCK_MONOTONIC_COARSE: i32 = 6;

fn clock(id: i32) -> Timespec {
    let mut time = Timespec { seconds: 0, nanoseconds: 0 };
    // SAFETY: writing one timespec; these clocks always exist on Linux.
    unsafe { clock_gettime(id, &mut time) };
    time
}

/// Monotonic time in nanoseconds at the kernel tick's resolution (1 to 4 ms). Reading it costs
/// about 2 ns against 20 ns for the precise clock, and I/O deadlines (whole seconds, swept every
/// quarter second) need no better.
pub(crate) fn coarse_now() -> u64 {
    let time = clock(CLOCK_MONOTONIC_COARSE);
    time.seconds as u64 * 1_000_000_000 + time.nanoseconds as u64
}

/// Seconds since 1970, at the kernel tick's resolution: for the HTTP `Date` header.
pub(crate) fn coarse_unix_seconds() -> u64 {
    clock(CLOCK_REALTIME_COARSE).seconds as u64
}

/// An I/O deadline (on the [`coarse_now`] clock) `after` from now.
pub(crate) fn deadline_after(after: Duration) -> u64 {
    coarse_now().saturating_add(after.as_nanos() as u64)
}

/// The I/O deadline for an `Instant`, for callers that keep their deadlines as `Instant`s.
pub(crate) fn deadline_at(at: Instant) -> u64 {
    deadline_after(at.saturating_duration_since(Instant::now()))
}

#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

struct Poller {
    epfd: i32,
    wake_fd: i32,
}

static POLLER: OnceLock<Poller> = OnceLock::new();

/// The token of the wake-up eventfd; sources use their address.
const WAKE_TOKEN: u64 = 0;

pub(crate) fn init() {
    POLLER.get_or_init(|| {
        // SAFETY: plain system calls.
        let epfd = unsafe { epoll_create1(EPOLL_CLOEXEC) };
        let wake_fd = unsafe { eventfd(0, EFD_NONBLOCK | EFD_CLOEXEC) };
        if epfd < 0 || wake_fd < 0 {
            crate::out::flush();
            eprintln!("fatal: could not create the network poller");
            std::process::abort();
        }
        let mut event = EpollEvent { events: EPOLLIN | EPOLLET, data: WAKE_TOKEN };
        // SAFETY: registering our own eventfd.
        unsafe { epoll_ctl(epfd, EPOLL_CTL_ADD, wake_fd, &mut event) };
        Poller { epfd, wake_fd }
    });
}

/// Wake one worker waiting in [`poll`].
pub(crate) fn notify() {
    if let Some(poller) = POLLER.get() {
        let one: u64 = 1;
        // SAFETY: writing eight bytes to our eventfd.
        unsafe { write(poller.wake_fd, &one as *const u64 as *const std::ffi::c_void, 8) };
    }
}

/// Wait up to `timeout_ms` (-1: until something happens) for sockets to become ready, and wake
/// their waiters.
pub(crate) fn poll(timeout_ms: i32) {
    let Some(poller) = POLLER.get() else { return };
    let mut events = [EpollEvent { events: 0, data: 0 }; 64];
    // SAFETY: a buffer of 64 events.
    let n = unsafe { epoll_wait(poller.epfd, events.as_mut_ptr(), events.len() as i32, timeout_ms) };
    for event in events.iter().take(n.max(0) as usize) {
        let (flags, data) = (event.events, event.data);
        if data == WAKE_TOKEN {
            let mut count = 0u64;
            // SAFETY: reading our eventfd's counter.
            unsafe { read(poller.wake_fd, &mut count as *mut u64 as *mut std::ffi::c_void, 8) };
            fiber::wake_taken();
            continue;
        }
        // SAFETY: sources are never freed (see above).
        let source = unsafe { &*(data as *const IoSource) };
        if flags & (EPOLLIN | EPOLLRDHUP | EPOLLHUP | EPOLLERR) != 0 {
            source.read.ready();
        }
        if flags & (EPOLLOUT | EPOLLHUP | EPOLLERR) != 0 {
            source.write.ready();
        }
    }
}

// ---- I/O sources ----------------------------------------------------------------------

/// One direction of a source: an edge that nobody has consumed yet, who waits for one, and
/// until when.
///
/// An edge either wakes the waiter or, when there is none, sets `ready` for the next `wait` to
/// find. Both sides decide under the `waiter` mutex, so an edge is never lost and never counted
/// twice: before docs/10 milestone 2 an edge did both, and the woken task's next `wait`
/// returned at once, costing a `recv` that found nothing on every request.
struct Direction {
    ready: AtomicBool,
    waiter: Mutex<Option<Waker>>,
    /// The waiter's deadline on the [`coarse_now`] clock; 0 for none. Written and checked under
    /// the `waiter` mutex.
    deadline: AtomicU64,
    /// Set by the deadline sweep when it wakes the waiter because its deadline passed.
    timed_out: AtomicBool,
}

impl Direction {
    const fn new() -> Direction {
        Direction {
            ready: AtomicBool::new(false),
            waiter: Mutex::new(None),
            deadline: AtomicU64::new(0),
            timed_out: AtomicBool::new(false),
        }
    }

    fn ready(&self) {
        let waker = {
            let mut waiter = lock(&self.waiter);
            let waker = waiter.take();
            if waker.is_none() {
                self.ready.store(true, Ordering::SeqCst);
            }
            waker
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    fn reset(&self) {
        let mut waiter = lock(&self.waiter);
        waiter.take();
        self.ready.store(false, Ordering::SeqCst);
        self.deadline.store(0, Ordering::SeqCst);
        self.timed_out.store(false, Ordering::SeqCst);
    }
}

/// A socket registered with the poller.
pub(crate) struct IoSource {
    fd: AtomicI32,
    read: Direction,
    write: Direction,
}

/// Every source ever made, for recycling and for waking them all at shutdown.
static SOURCES: Mutex<Vec<&'static IoSource>> = Mutex::new(Vec::new());
static FREE_SOURCES: Mutex<Vec<&'static IoSource>> = Mutex::new(Vec::new());

/// Why a wait ended.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Wait {
    /// Ready, or woken for another reason: try the operation again.
    Retry,
    /// The deadline passed.
    TimedOut,
}

impl IoSource {
    /// Register a non-blocking socket. Returns `None` outside the fiber runtime, where waits use
    /// `poll(2)` instead.
    pub(crate) fn register(fd: RawFd) -> Option<&'static IoSource> {
        let poller = POLLER.get()?;
        let source = lock(&FREE_SOURCES).pop().unwrap_or_else(|| {
            let source: &'static IoSource = Box::leak(Box::new(IoSource {
                fd: AtomicI32::new(-1),
                read: Direction::new(),
                write: Direction::new(),
            }));
            lock(&SOURCES).push(source);
            source
        });
        source.fd.store(fd, Ordering::SeqCst);
        let mut event =
            EpollEvent { events: EPOLLIN | EPOLLOUT | EPOLLRDHUP | EPOLLET, data: source as *const IoSource as u64 };
        // SAFETY: registering a socket the caller owns.
        if unsafe { epoll_ctl(poller.epfd, EPOLL_CTL_ADD, fd, &mut event) } != 0 {
            source.fd.store(-1, Ordering::SeqCst);
            lock(&FREE_SOURCES).push(source);
            return None;
        }
        Some(source)
    }

    /// Stop watching the socket (before it is closed) and recycle the source.
    pub(crate) fn deregister(&'static self) {
        let fd = self.fd.swap(-1, Ordering::SeqCst);
        if let Some(poller) = POLLER.get()
            && fd >= 0
        {
            let mut event = EpollEvent { events: 0, data: 0 };
            // SAFETY: removing a registration this source made.
            unsafe { epoll_ctl(poller.epfd, EPOLL_CTL_DEL, fd, &mut event) };
        }
        self.read.reset();
        self.write.reset();
        lock(&FREE_SOURCES).push(self);
    }

    /// Wait until the socket may be readable (`write` false) or writable, or until `deadline`
    /// (on the [`coarse_now`] clock). A deadline that has already passed is noticed by the next
    /// sweep, within a quarter of a second; no clock is read here.
    pub(crate) fn wait(&self, write: bool, deadline: Option<u64>) -> Wait {
        let direction = if write { &self.write } else { &self.read };
        {
            let mut waiter = lock(&direction.waiter);
            if direction.ready.swap(false, Ordering::SeqCst) {
                return Wait::Retry;
            }
            direction.timed_out.store(false, Ordering::SeqCst);
            direction.deadline.store(deadline.map_or(0, |d| d.max(1)), Ordering::SeqCst);
            *waiter = Some(Waker::current());
        }
        // A wake that comes before the park leaves a permit, so it is not lost.
        fiber::park(None);
        lock(&direction.waiter).take();
        direction.deadline.store(0, Ordering::SeqCst);
        if direction.timed_out.swap(false, Ordering::SeqCst) { Wait::TimedOut } else { Wait::Retry }
    }
}

/// Wait for `fd` with `poll(2)`, for a thread that is not running fibers.
pub(crate) fn wait_blocking(fd: RawFd, write: bool, deadline: Option<u64>) -> Wait {
    let timeout = match deadline {
        None => 500,
        Some(d) => {
            let left = d.saturating_sub(coarse_now());
            if left == 0 {
                return Wait::TimedOut;
            }
            // Rounded up, so that the deadline has passed when the wait ends.
            left.div_ceil(1_000_000).min(500) as i32
        }
    };
    let mut pfd = PollFd { fd, events: if write { POLLOUT } else { POLLIN }, revents: 0 };
    // SAFETY: one valid pollfd.
    unsafe { sys_poll(&mut pfd, 1, timeout) };
    if deadline.is_some_and(|d| coarse_now() >= d) { Wait::TimedOut } else { Wait::Retry }
}

/// Wake every task waiting on a socket, so it notices a shutdown.
pub(crate) fn wake_all_io() {
    for source in lock(&SOURCES).iter() {
        for direction in [&source.read, &source.write] {
            if let Some(waker) = lock(&direction.waiter).take() {
                waker.wake();
            }
        }
    }
}

// ---- timers -----------------------------------------------------------------------------

struct Timers {
    heap: Mutex<BinaryHeap<Reverse<(Instant, u64, TimerFiber)>>>,
    changed: Condvar,
    sequence: AtomicU64,
}

struct TimerFiber(FiberRef);

impl PartialEq for TimerFiber {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for TimerFiber {}
impl PartialOrd for TimerFiber {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for TimerFiber {
    fn cmp(&self, _: &Self) -> std::cmp::Ordering {
        std::cmp::Ordering::Equal
    }
}

static TIMERS: Timers =
    Timers { heap: Mutex::new(BinaryHeap::new()), changed: Condvar::new(), sequence: AtomicU64::new(0) };

/// Wake `fiber` at `deadline`.
pub(crate) fn add_timer(deadline: Instant, fiber: FiberRef) {
    let sequence = TIMERS.sequence.fetch_add(1, Ordering::Relaxed);
    let mut heap = lock(&TIMERS.heap);
    let earliest = heap.peek().is_none_or(|Reverse((first, _, _))| deadline < *first);
    heap.push(Reverse((deadline, sequence, TimerFiber(fiber))));
    if earliest {
        TIMERS.changed.notify_one();
    }
}

const SWEEP: Duration = Duration::from_millis(250);

pub(crate) fn start_timer_thread() {
    let started = std::thread::Builder::new().name("timers".into()).stack_size(128 * 1024).spawn(|| {
        let mut next_sweep = Instant::now() + SWEEP;
        loop {
            let mut due = Vec::new();
            {
                let mut heap = lock(&TIMERS.heap);
                let now = Instant::now();
                while let Some(Reverse((at, _, _))) = heap.peek() {
                    if *at > now {
                        break;
                    }
                    let Reverse((_, _, TimerFiber(fiber))) = heap.pop().expect("peeked");
                    due.push(fiber);
                }
                if due.is_empty() && now < next_sweep {
                    let until = heap.peek().map_or(next_sweep, |Reverse((at, _, _))| (*at).min(next_sweep));
                    let _ = TIMERS.changed.wait_timeout(heap, until.saturating_duration_since(now));
                    continue;
                }
            }
            for fiber in due {
                fiber::unpark(&fiber);
            }
            let now = Instant::now();
            if now >= next_sweep {
                next_sweep = now + SWEEP;
                sweep_io_deadlines(coarse_now());
            }
        }
    });
    if let Err(error) = started {
        crate::out::flush();
        eprintln!("fatal: could not start the timer thread: {error}");
        std::process::abort();
    }
}

fn sweep_io_deadlines(now: u64) {
    let sources: Vec<&'static IoSource> = lock(&SOURCES).clone();
    for source in sources {
        for direction in [&source.read, &source.write] {
            // A cheap look first; then again under the mutex, where the deadline belongs to
            // the waiter that is there (it may have changed since).
            let deadline = direction.deadline.load(Ordering::Relaxed);
            if deadline == 0 || deadline > now {
                continue;
            }
            let waker = {
                let mut waiter = lock(&direction.waiter);
                let deadline = direction.deadline.load(Ordering::SeqCst);
                if deadline == 0 || deadline > now {
                    continue;
                }
                let waker = waiter.take();
                if waker.is_some() {
                    direction.timed_out.store(true, Ordering::SeqCst);
                }
                waker
            };
            if let Some(waker) = waker {
                waker.wake();
            }
        }
    }
}
