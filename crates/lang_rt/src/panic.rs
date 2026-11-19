//! Panics: the program has a bug. Print the message and where it happened.
//!
//! On the main thread a panic ends the process with status 101. In a task (a fiber) it ends only
//! that task (docs/04-runtime-and-memory.md, "Panics"): the runtime unwinds to the fiber's entry,
//! which marks the task as panicked and releases the mutexes it held. An HTTP handler that
//! panics is answered with a 500, and the server carries on.

use crate::{Str, fiber, location};

/// Exit status of a program that panicked.
pub const PANIC_EXIT_CODE: i32 = 101;

/// What a task's unwinding panic carries; the message has already been printed.
pub(crate) struct TaskPanic;

pub fn panic_at(message: &str, location: &str) -> ! {
    crate::out::flush();
    let at = if location.is_empty() { String::new() } else { format!("\n  --> {location}") };
    let trace = backtrace();
    match fiber::current() {
        Some(task) => {
            eprintln!("panic in task {}: {message}{at}{trace}", task.id);
            std::panic::resume_unwind(Box::new(TaskPanic))
        }
        None => {
            eprintln!("panic: {message}{at}{trace}");
            std::process::exit(PANIC_EXIT_CODE)
        }
    }
}

/// The calls that led here, in the program's own source: one line per frame whose debug
/// information points at a source file of the language, innermost first. Empty when
/// `<PREFIX>BACKTRACE=0`, or when there is no such frame (a program built without debug
/// information).
fn backtrace() -> String {
    if std::env::var(lang_names::env_var("BACKTRACE")).is_ok_and(|v| v == "0") {
        return String::new();
    }
    let text = std::backtrace::Backtrace::force_capture().to_string();
    let suffix = format!(".{}:", lang_names::SOURCE_EXTENSION);
    // The standard library prints `N: symbol` lines, each followed by `at file:line:col`.
    let mut frames = Vec::new();
    let mut symbol = "";
    for line in text.lines() {
        let line = line.trim();
        if let Some(at) = line.strip_prefix("at ") {
            if at.contains(&suffix) {
                // Symbols are `module.function` or `module.Type.method`; the entry point is
                // the program's `main`.
                let name = match symbol {
                    "lang_user_main" => "main",
                    _ => symbol.split_once('.').map_or(symbol, |(_, name)| name),
                };
                frames.push(format!("    {name} at {}", at.trim_start_matches("./")));
            }
        } else if let Some((_, rest)) = line.split_once(": ") {
            symbol = rest;
        }
    }
    // The innermost frame is where the panic happened, which the message already says.
    if frames.len() < 2 {
        return String::new();
    }
    format!("\nbacktrace:\n{}", frames.join("\n"))
}

/// # Safety
/// Both pointer and length pairs must describe UTF-8 strings.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_panic(msg: *const u8, msg_len: i64, loc: *const u8, loc_len: i64) -> ! {
    // SAFETY: guaranteed by the caller.
    unsafe { panic_at(Str::from_raw(msg, msg_len), location(loc, loc_len)) }
}

/// # Safety
/// `loc` and `loc_len` must describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_panic_index(index: i64, len: i64, loc: *const u8, loc_len: i64) -> ! {
    let message = format!("index {index} is out of range for a list of length {len}");
    // SAFETY: guaranteed by the caller.
    panic_at(&message, unsafe { location(loc, loc_len) })
}
