//! A message for stack overflow (docs/11, milestone 5).
//!
//! Running out of stack used to end a program with a bare "Segmentation fault". Now the
//! runtime installs a handler for SIGSEGV and SIGBUS, on an alternate stack of each thread that
//! runs program code (the thread's own stack is the one that is full). If the faulting address
//! is where a stack ran out, it prints `panic: stack overflow` and exits with status 101, the
//! status of a panic. Anything else is a real bug: the handler restores the default action and
//! returns, so the access faults again and the program dies as it would have.
//!
//! "Where a stack ran out" is: for a task, a page of the fiber's stack guard or just beside it;
//! for the main thread, near the end of the stack the system allows it. The handler only does
//! what is safe in a signal handler: reads of thread-local and atomic data, `write` and `_exit`.
//! Unwinding is not safe there, so a task's overflow ends the whole program, as every panic in
//! a task unwinds only to the task's entry, which the stack no longer allows.

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

const SIGBUS: i32 = 7;
const SIGSEGV: i32 = 11;
const SA_SIGINFO: i32 = 4;
const SA_ONSTACK: i32 = 0x0800_0000;
const SA_NODEFER: i32 = 0x4000_0000;
const SIG_DFL: usize = 0;
/// Bytes of alternate stack for each thread, enough for the handler and the kernel's signal frame.
const ALT_STACK: usize = 64 * 1024;
/// How far from the end of a stack an access still counts as running off it.
const WINDOW: usize = 64 * 1024;

/// glibc's `struct sigaction` on x86-64 Linux.
#[repr(C)]
struct SigAction {
    handler: usize,
    mask: [u64; 16],
    flags: i32,
    restorer: usize,
}

/// `stack_t`.
#[repr(C)]
struct AltStack {
    sp: *mut u8,
    flags: i32,
    size: usize,
}

unsafe extern "C" {
    fn sigaction(signal: i32, action: *const SigAction, old: *mut SigAction) -> i32;
    fn sigaltstack(new: *const AltStack, old: *mut AltStack) -> i32;
    fn write(fd: i32, data: *const c_void, len: usize) -> isize;
    fn _exit(status: i32) -> !;
    fn raise(signal: i32) -> i32;
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, offset: i64) -> *mut u8;
    fn pthread_self() -> usize;
    fn pthread_getattr_np(thread: usize, attr: *mut u8) -> i32;
    fn pthread_attr_getstack(attr: *const u8, addr: *mut *mut u8, size: *mut usize) -> i32;
    fn pthread_attr_destroy(attr: *mut u8) -> i32;
}

/// The lowest address of the main thread's stack, and the messages the handler prints, set up
/// before the handler can run.
static MAIN_STACK_LOW: AtomicUsize = AtomicUsize::new(0);
static MAIN_MESSAGE: Message = Message::new();
static TASK_MESSAGE: Message = Message::new();

/// Text made at start-up (it needs the environment variable's name) and read by the handler.
struct Message {
    ptr: AtomicUsize,
    len: AtomicUsize,
}

impl Message {
    const fn new() -> Message {
        Message { ptr: AtomicUsize::new(0), len: AtomicUsize::new(0) }
    }

    fn set(&self, text: String) {
        let text: &'static str = Box::leak(text.into_boxed_str());
        self.len.store(text.len(), Ordering::Release);
        self.ptr.store(text.as_ptr() as usize, Ordering::Release);
    }

    fn write_to_stderr(&self) {
        let (ptr, len) = (self.ptr.load(Ordering::Acquire), self.len.load(Ordering::Acquire));
        if ptr != 0 {
            // SAFETY: a leaked string.
            unsafe { write(2, ptr as *const c_void, len) };
        }
    }
}

/// Install the handler, and an alternate stack for this thread. Called once, first thing in the
/// program.
pub fn install() {
    let variable = lang_names::env_var("TASK_STACK");
    MAIN_MESSAGE.set(
        "panic: stack overflow\n  the program ran out of stack, which is usually a function that calls itself without end\n"
            .to_string(),
    );
    TASK_MESSAGE.set(format!(
        "panic: stack overflow in a task\n  the task ran out of stack (1 MiB unless {variable} gives another size in bytes), which is usually a function that calls itself without end\n"
    ));
    MAIN_STACK_LOW.store(main_stack_low(), Ordering::Release);
    setup_thread();
    for signal in [SIGSEGV, SIGBUS] {
        let action = SigAction {
            handler: handle as *const () as usize,
            mask: [0; 16],
            flags: SA_SIGINFO | SA_ONSTACK | SA_NODEFER,
            restorer: 0,
        };
        // SAFETY: a valid action for a real signal; the handler is async-signal-safe.
        unsafe { sigaction(signal, &action, std::ptr::null_mut()) };
    }
}

/// Give the calling thread an alternate stack for the handler to run on. A thread that runs
/// program code calls this once, when it starts.
pub fn setup_thread() {
    // SAFETY: a fresh anonymous mapping that lives as long as the thread, which is the process.
    let memory = unsafe { mmap(std::ptr::null_mut(), ALT_STACK, 3, 0x22, -1, 0) };
    if memory as isize == -1 {
        return;
    }
    let stack = AltStack { sp: memory, flags: 0, size: ALT_STACK };
    // SAFETY: the mapping is ours and stays mapped.
    unsafe { sigaltstack(&stack, std::ptr::null_mut()) };
}

/// The lowest address of the main thread's stack, which is as far as the system lets it grow.
fn main_stack_low() -> usize {
    let mut attributes = [0u8; 128];
    let (mut address, mut size) = (std::ptr::null_mut(), 0usize);
    // SAFETY: `attributes` is larger than a `pthread_attr_t`; the calls only fill it in.
    unsafe {
        if pthread_getattr_np(pthread_self(), attributes.as_mut_ptr()) != 0 {
            return 0;
        }
        let found = pthread_attr_getstack(attributes.as_ptr(), &mut address, &mut size) == 0;
        pthread_attr_destroy(attributes.as_mut_ptr());
        if found { address as usize } else { 0 }
    }
}

/// The stack pointer of the interrupted code, from the signal's saved registers (x86-64).
fn interrupted_stack_pointer(context: *const c_void) -> usize {
    // `ucontext_t`: flags and link (16 bytes), the alternate stack (24), then the registers,
    // of which `REG_RSP` is the sixteenth.
    const RSP_OFFSET: usize = 16 + 24 + 15 * 8;
    // SAFETY: the kernel passes a full context.
    unsafe { *(context as *const u8).add(RSP_OFFSET).cast::<usize>() }
}

extern "C" fn handle(signal: i32, info: *const c_void, context: *const c_void) {
    // `si_addr` is at offset 16 of `siginfo_t` on 64-bit Linux.
    // SAFETY: the kernel passes a full `siginfo_t`.
    let address = unsafe { *(info as *const u8).add(16).cast::<usize>() };
    if let Some((low, high)) = crate::fiber::guard_window()
        && (low..high).contains(&address)
    {
        finish(&TASK_MESSAGE);
    }
    let main_low = MAIN_STACK_LOW.load(Ordering::Acquire);
    if main_low != 0 {
        let stack_pointer = interrupted_stack_pointer(context);
        let near_the_end = address >= main_low.saturating_sub(WINDOW) && address < main_low + WINDOW;
        let near_the_pointer = address.abs_diff(stack_pointer) <= WINDOW;
        if near_the_end && near_the_pointer && crate::fiber::guard_window().is_none() {
            finish(&MAIN_MESSAGE);
        }
    }
    // Not a stack overflow: restore the default action. A fault happens again when the
    // instruction runs again; a signal sent from outside (`si_code` zero or negative) must be
    // delivered again.
    let action = SigAction { handler: SIG_DFL, mask: [0; 16], flags: 0, restorer: 0 };
    // SAFETY: restoring the default action, and raising the signal, which is allowed in a handler.
    unsafe {
        sigaction(signal, &action, std::ptr::null_mut());
        let code = *(info as *const u8).add(8).cast::<i32>();
        if code <= 0 {
            raise(signal);
        }
    }
}

/// Print `message`, after what the program had printed, and exit as a panic does.
fn finish(message: &Message) -> ! {
    crate::out::try_flush();
    message.write_to_stderr();
    // SAFETY: ends the process without running the program's exit handlers.
    unsafe { _exit(101) }
}
