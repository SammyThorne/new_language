//! Regular expressions for the `re` module, on the `regex-lite` crate: Perl-like syntax without
//! look-around or backreferences, and matching time linear in the length of the text.
//!
//! Compiled expressions live in a table for the life of the program and are named by their
//! index; a `re.Regex` value holds the index, so it can be shared between tasks. The positions of
//! the last match are kept per thread and read straight after the call that found them (no
//! task switch can come in between).

use crate::{Str, sys};
use regex_lite::Regex;
use std::cell::RefCell;
use std::sync::{Arc, RwLock};

static TABLE: RwLock<Vec<Arc<Regex>>> = RwLock::new(Vec::new());

thread_local! {
    /// Byte offsets from the last call: groups of a match, all matches, or split pieces.
    static SPANS: RefCell<Vec<(i64, i64)>> = const { RefCell::new(Vec::new()) };
}

fn regex(id: i64) -> Arc<Regex> {
    let table = TABLE.read().unwrap_or_else(|p| p.into_inner());
    table[id as usize].clone()
}

fn set_spans(spans: Vec<(i64, i64)>) -> i64 {
    let count = spans.len() as i64;
    SPANS.with(|s| *s.borrow_mut() = spans);
    count
}

/// Compile a pattern; its id, or -1 after recording the error.
///
/// # Safety
/// The pointer and length describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_re_compile(ptr: *const u8, len: i64) -> i64 {
    match Regex::new(unsafe { Str::from_raw(ptr, len) }) {
        Ok(regex) => {
            let mut table = TABLE.write().unwrap_or_else(|p| p.into_inner());
            table.push(Arc::new(regex));
            table.len() as i64 - 1
        }
        Err(error) => {
            sys::record_error(error.to_string().lines().last().unwrap_or("invalid pattern").trim());
            -1
        }
    }
}

/// # Safety
/// The pointer and length describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_re_is_match(id: i64, ptr: *const u8, len: i64) -> i64 {
    i64::from(regex(id).is_match(unsafe { Str::from_raw(ptr, len) }))
}

/// The first match at or after byte `start`: returns the number of groups (group 0 is the whole
/// match), with their spans (-1, -1 for a group that did not take part), or 0 for no match.
///
/// # Safety
/// The pointer and length describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_re_find(id: i64, ptr: *const u8, len: i64, start: i64) -> i64 {
    let text = unsafe { Str::from_raw(ptr, len) };
    let start = (start.max(0) as usize).min(text.len());
    if !text.is_char_boundary(start) {
        return 0;
    }
    let regex = regex(id);
    let Some(captures) = regex.captures_at(text, start) else { return set_spans(Vec::new()) };
    let spans =
        (0..captures.len()).map(|i| captures.get(i).map_or((-1, -1), |m| (m.start() as i64, m.end() as i64))).collect();
    set_spans(spans)
}

/// Every non-overlapping match: returns how many, with their spans.
///
/// # Safety
/// The pointer and length describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_re_find_all(id: i64, ptr: *const u8, len: i64) -> i64 {
    let text = unsafe { Str::from_raw(ptr, len) };
    set_spans(regex(id).find_iter(text).map(|m| (m.start() as i64, m.end() as i64)).collect())
}

/// The pieces of the text between matches: returns how many, with their spans.
///
/// # Safety
/// The pointer and length describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_re_split(id: i64, ptr: *const u8, len: i64) -> i64 {
    let text = unsafe { Str::from_raw(ptr, len) };
    let mut spans = Vec::new();
    let mut last = 0;
    for m in regex(id).find_iter(text) {
        spans.push((last as i64, m.start() as i64));
        last = m.end();
    }
    spans.push((last as i64, text.len() as i64));
    set_spans(spans)
}

/// A span from the last call: its start (`end` = 0) or end (`end` = 1).
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_re_span(index: i64, end: i64) -> i64 {
    SPANS.with(|s| s.borrow().get(index as usize).map_or(-1, |&(a, b)| if end != 0 { b } else { a }))
}

/// The number of a named group, or -1.
///
/// # Safety
/// The pointer and length describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_re_group_index(id: i64, ptr: *const u8, len: i64) -> i64 {
    let name = unsafe { Str::from_raw(ptr, len) };
    regex(id).capture_names().position(|n| n == Some(name)).map_or(-1, |i| i as i64)
}

/// Replace the first match (`all` = 0) or every match. `$1` and `${name}` in the replacement
/// insert groups; `$$` is a dollar sign.
///
/// # Safety
/// The pointers and lengths describe UTF-8 strings.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_re_replace(
    id: i64,
    ptr: *const u8,
    len: i64,
    rep_ptr: *const u8,
    rep_len: i64,
    all: i64,
) -> Str {
    let text = unsafe { Str::from_raw(ptr, len) };
    let replacement = unsafe { Str::from_raw(rep_ptr, rep_len) };
    let regex = regex(id);
    let result = if all != 0 { regex.replace_all(text, replacement) } else { regex.replace(text, replacement) };
    Str::from_bytes(result.as_bytes())
}
