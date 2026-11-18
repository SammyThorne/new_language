//! Panics: the program has a bug. Print the message and where it happened, then exit.
//!
//! Until tasks exist, a panic ends the process (docs/04-runtime-and-memory.md, "Panics").

use crate::{Str, location};

/// Exit status of a program that panicked.
pub const PANIC_EXIT_CODE: i32 = 101;

pub fn panic_at(message: &str, location: &str) -> ! {
    crate::out::flush();
    eprintln!("panic: {message}\n  --> {location}");
    std::process::exit(PANIC_EXIT_CODE)
}

/// # Safety
/// Both pointer and length pairs must describe UTF-8 strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lrt_panic(msg: *const u8, msg_len: i64, loc: *const u8, loc_len: i64) -> ! {
    // SAFETY: guaranteed by the caller.
    unsafe { panic_at(Str::from_raw(msg, msg_len), location(loc, loc_len)) }
}

/// # Safety
/// `loc` and `loc_len` must describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lrt_panic_index(index: i64, len: i64, loc: *const u8, loc_len: i64) -> ! {
    let message = format!("index {index} is out of range for a list of length {len}");
    // SAFETY: guaranteed by the caller.
    panic_at(&message, unsafe { location(loc, loc_len) })
}
