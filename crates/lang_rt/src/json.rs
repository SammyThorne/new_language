//! The JSON scanner and writer (docs/05-standard-library.md, "JSON").
//!
//! Generated decoders drive a [`Reader`] one token at a time: they ask for a number, a string,
//! the next element of an array or the next key of an object, and the reader checks the syntax
//! as it goes (RFC 8259, strictly: no comments, no trailing commas, no single quotes, no leading
//! zeros, no lone surrogates). The first error is kept, with the path to where it happened
//! (`addresses[1].zip`), and every later call fails quickly.
//!
//! The reader is a fixed-size record in the generated function's stack frame
//! (`lrt_json_reader_init`), so a decode makes no Rust heap allocations unless it fails or nests
//! more than `INLINE_PATH` deep. Strings without escapes are copied once, straight from the
//! input into collected memory; object keys that are only compared with field names are not
//! copied at all (`lrt_json_object_key_view`).
//!
//! The `config` module decodes in a lenient mode (`lrt_json_set_lenient`), over an object whose
//! values are the text of environment variables: a string is accepted where a number, a
//! boolean, an array or an object is expected and read from its text, and a field of the
//! top-level object that fails is recorded and skipped, so that one decode reports every
//! problem (`lrt_json_recover`, `lrt_json_missing`, `lrt_json_problem`).

use std::cell::{Cell, RefCell};

use crate::Str;
use crate::builder::{Builder, lrt_builder_push};

thread_local! {
    /// Whether the next reader set up on this thread is lenient.
    static LENIENT: Cell<bool> = const { Cell::new(false) };
    /// The problems the last lenient decode on this thread found: (path, message).
    static PROBLEMS: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) };
}

/// Nesting deeper than this is an error, so that hostile input cannot exhaust the stack.
const MAX_DEPTH: usize = 512;

/// The bytes generated code reserves on its stack for a [`Reader`] (`JSON_READER_BYTES` in
/// `lang_codegen`, which must match).
pub const READER_BYTES: usize = 512;

/// Path parts kept in the reader itself; deeper ones go to a `Vec`.
const INLINE_PATH: usize = 16;

/// One step of the path to the value being read: an object key (`ptr`, `len`) or, with a null
/// `ptr`, an array index (`len`). Keys are field names (constants) or dict keys that the
/// generated decoder holds until it pops them, so the pointers stay valid.
#[derive(Clone, Copy)]
struct PathPart {
    ptr: *const u8,
    len: i64,
}

#[repr(C)]
pub struct Reader {
    input: &'static [u8],
    pos: usize,
    /// How many arrays and objects are open.
    depth: usize,
    /// One bit per open array or object, set while no element has been read from it yet.
    first: [u64; MAX_DEPTH / 64],
    path_len: usize,
    path: [PathPart; INLINE_PATH],
    deeper_path: Vec<PathPart>,
    /// The message and the path of the first error.
    error: Option<Box<(String, String)>>,
    /// Lenient mode, for `config` (see the module documentation).
    lenient: bool,
    /// In lenient mode, the errors in top-level fields that were recovered from.
    problems: Vec<(String, String)>,
    /// Inputs made by splicing a string's text into the input, all kept until the reader is
    /// freed: keys returned as views may point into any of them.
    buffers: Vec<Box<[u8]>>,
    /// Where the value of the top-level field being read starts, and the depth there.
    top_start: usize,
    top_depth: usize,
}

const _: () = assert!(std::mem::size_of::<Reader>() <= READER_BYTES);
const _: () = assert!(std::mem::align_of::<Reader>() <= 8);

impl Reader {
    fn new(input: &'static [u8]) -> Reader {
        Reader {
            input,
            pos: 0,
            depth: 0,
            first: [0; MAX_DEPTH / 64],
            path_len: 0,
            path: [PathPart { ptr: std::ptr::null(), len: 0 }; INLINE_PATH],
            deeper_path: Vec::new(),
            error: None,
            lenient: false,
            problems: Vec::new(),
            buffers: Vec::new(),
            top_start: 0,
            top_depth: 0,
        }
    }

    fn push_path(&mut self, part: PathPart) {
        if self.path_len == 0 {
            self.top_start = skip_ws(self.input, self.pos);
            self.top_depth = self.depth;
        }
        if self.path_len < INLINE_PATH {
            self.path[self.path_len] = part;
        } else {
            self.deeper_path.push(part);
        }
        self.path_len += 1;
    }

    fn pop_path(&mut self) {
        if self.path_len > INLINE_PATH {
            self.deeper_path.pop();
        }
        self.path_len = self.path_len.saturating_sub(1);
    }

    fn clear_path(&mut self) {
        self.path_len = 0;
        self.deeper_path.clear();
    }

    fn fail(&mut self, message: impl Into<String>) {
        if self.error.is_none() {
            let mut path = String::new();
            let parts = self.path[..self.path_len.min(INLINE_PATH)].iter().chain(&self.deeper_path);
            for part in parts {
                if part.ptr.is_null() {
                    path.push_str(&format!("[{}]", part.len));
                    continue;
                }
                if !path.is_empty() {
                    path.push('.');
                }
                // SAFETY: key parts point to live UTF-8 strings (see `PathPart`).
                path.push_str(unsafe { Str::from_raw(part.ptr, part.len) });
            }
            self.error = Some(Box::new((message.into(), path)));
        }
    }

    fn failed(&self) -> bool {
        self.error.is_some()
    }

    fn skip_ws(&mut self) {
        self.pos = skip_ws(self.input, self.pos);
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    /// What the value at the current position looks like, for error messages.
    fn found(&self) -> String {
        match self.peek() {
            None => "the end of the input".to_string(),
            Some(b'{') => "an object".to_string(),
            Some(b'[') => "an array".to_string(),
            Some(b'"') => "a string".to_string(),
            Some(b't' | b'f') => "a boolean".to_string(),
            Some(b'n') => "null".to_string(),
            Some(b'-' | b'0'..=b'9') => "a number".to_string(),
            Some(c) => format!("`{}`", char::from(c)),
        }
    }

    fn expected(&mut self, what: &str) {
        let found = self.found();
        self.fail(format!("expected {what}, found {found}"));
    }

    /// Consume `word` (`null`, `true`, `false`) if it is next.
    fn literal(&mut self, word: &[u8]) -> bool {
        if self.input[self.pos..].starts_with(word) {
            self.pos += word.len();
            return true;
        }
        false
    }

    fn string(&mut self) -> Option<Text<'static>> {
        self.skip_ws();
        if self.peek() != Some(b'"') {
            self.expected("a string");
            return None;
        }
        match scan_string(self.input, self.pos) {
            Ok((text, end)) => {
                self.pos = end;
                Some(text)
            }
            Err((message, at)) => {
                self.pos = at;
                self.fail(message);
                None
            }
        }
    }

    /// The next key of the open object, with its `:` consumed; `None` at the end of the object
    /// or on error.
    fn key(&mut self) -> Option<Text<'static>> {
        if self.next(b'}') != 1 {
            return None;
        }
        let key = self.string()?;
        self.skip_ws();
        if self.peek() != Some(b':') {
            self.expected("`:` after the key");
            return None;
        }
        self.pos += 1;
        Some(key)
    }

    /// Replace the input between `start` and `end` with `replacement`.
    fn splice(&mut self, start: usize, end: usize, replacement: &[u8]) {
        let mut buffer = Vec::with_capacity(self.input.len() - (end - start) + replacement.len());
        buffer.extend_from_slice(&self.input[..start]);
        buffer.extend_from_slice(replacement);
        buffer.extend_from_slice(&self.input[end..]);
        let buffer = buffer.into_boxed_slice();
        // SAFETY: the box is kept in `buffers`, which only grows, until the reader is dropped;
        // moving the box does not move its bytes.
        self.input = unsafe { std::slice::from_raw_parts(buffer.as_ptr(), buffer.len()) };
        self.buffers.push(buffer);
    }

    /// In lenient mode, a string where a number or boolean is expected: its trimmed text, with
    /// the string consumed. `None` when the next value is not a string (or not lenient).
    fn lenient_text(&mut self) -> Option<String> {
        if !self.lenient || self.failed() {
            return None;
        }
        self.skip_ws();
        if self.peek() != Some(b'"') {
            return None;
        }
        let text = self.string()?;
        Some(String::from_utf8_lossy(text.bytes()).trim().to_string())
    }

    /// In lenient mode, a string where an array or object (`open`) is expected becomes one: its
    /// text is spliced in if it is JSON of that kind, and for an array, otherwise it is split
    /// on commas into strings. Records an error if it is neither.
    fn lenient_open(&mut self, open: u8) {
        if !self.lenient || self.failed() {
            return;
        }
        self.skip_ws();
        if self.peek() != Some(b'"') {
            return;
        }
        let start = self.pos;
        let Ok((text, end)) = scan_string(self.input, start) else { return };
        let text = String::from_utf8_lossy(text.bytes()).into_owned();
        let trimmed = text.trim();
        let replacement = if trimmed.as_bytes().first() == Some(&open) {
            match skip(trimmed.as_bytes(), 0, self.depth) {
                Ok(n) if skip_ws(trimmed.as_bytes(), n) == trimmed.len() => trimmed.as_bytes().to_vec(),
                Ok(n) => {
                    self.fail(format!(
                        "is not valid JSON: unexpected text at position {}",
                        skip_ws(trimmed.as_bytes(), n) + 1
                    ));
                    return;
                }
                Err((message, _)) => {
                    self.fail(format!("is not valid JSON: {message}"));
                    return;
                }
            }
        } else if open == b'[' {
            let mut list = b"[".to_vec();
            for item in trimmed.split(',').map(str::trim).filter(|item| !item.is_empty()) {
                if list.len() > 1 {
                    list.push(b',');
                }
                push_json_string(&mut list, item.as_bytes());
            }
            list.push(b']');
            list
        } else {
            self.fail(format!("must be a JSON object, got {}", shown(trimmed)));
            return;
        };
        self.splice(start, end, &replacement);
    }

    fn open(&mut self, open: u8, what: &str) -> bool {
        self.lenient_open(open);
        if self.failed() {
            return false;
        }
        self.skip_ws();
        if self.peek() != Some(open) {
            self.expected(what);
            return false;
        }
        if self.depth >= MAX_DEPTH {
            self.fail("the JSON is nested too deeply");
            return false;
        }
        self.pos += 1;
        self.first[self.depth / 64] |= 1 << (self.depth % 64);
        self.depth += 1;
        true
    }

    /// Before the next element of the open array or object: 1 if there is one, 0 at its end.
    fn next(&mut self, close: u8) -> i64 {
        if self.failed() {
            return -1;
        }
        self.skip_ws();
        let top = self.depth.checked_sub(1).expect("inside an array or object");
        let first = self.first[top / 64] & (1 << (top % 64)) != 0;
        if self.peek() == Some(close) {
            self.pos += 1;
            self.depth = top;
            return 0;
        }
        if !first {
            if self.peek() != Some(b',') {
                let what = if close == b']' { "`,` or `]`" } else { "`,` or `}`" };
                self.expected(what);
                return -1;
            }
            self.pos += 1;
            self.skip_ws();
            if self.peek() == Some(close) {
                self.fail("a trailing comma is not allowed");
                return -1;
            }
        }
        self.first[top / 64] &= !(1 << (top % 64));
        1
    }
}

/// A value from the environment in an error message: quoted, and cut short if it is long.
fn shown(text: &str) -> String {
    let mut chars = text.chars();
    let start: String = chars.by_ref().take(40).collect();
    if chars.next().is_some() { format!("{:?}...", start) } else { format!("{start:?}") }
}

/// The boolean spellings an environment variable may use.
fn lenient_bool(text: &str) -> Option<bool> {
    match text.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" => Some(true),
        "false" | "0" | "no" => Some(false),
        _ => None,
    }
}

fn skip_ws(input: &[u8], mut pos: usize) -> usize {
    while let Some(b' ' | b'\t' | b'\n' | b'\r') = input.get(pos) {
        pos += 1;
    }
    pos
}

fn hex4(input: &[u8], pos: usize) -> Option<u32> {
    let digits = input.get(pos..pos + 4)?;
    let text = std::str::from_utf8(digits).ok()?;
    if !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(text, 16).ok()
}

/// A string's text: a slice of the input when it has no escapes (almost always), or decoded.
/// Either way it is valid UTF-8, since the input is and escapes decode to scalar values.
enum Text<'a> {
    Raw(&'a [u8]),
    Decoded(Vec<u8>),
}

impl Text<'_> {
    fn bytes(&self) -> &[u8] {
        match self {
            Text::Raw(bytes) => bytes,
            Text::Decoded(bytes) => bytes,
        }
    }

    /// A copy in collected memory.
    fn to_str(&self) -> Str {
        Str::from_bytes(self.bytes())
    }
}

/// A string starting at the `"` at `pos`: its text and the position after its closing quote, or
/// an error message and where.
fn scan_string(input: &[u8], pos: usize) -> Result<(Text<'_>, usize), (String, usize)> {
    // The bytes that end a run of plain text are the ones a writer must escape.
    let end = next_escape(input, pos + 1);
    if input.get(end) == Some(&b'"') {
        return Ok((Text::Raw(&input[pos + 1..end]), end + 1));
    }
    let mut i = end;
    let mut out: Vec<u8> = input[pos + 1..end].to_vec();
    loop {
        let start = i;
        while let Some(&c) = input.get(i) {
            if c == b'"' || c == b'\\' || c < 0x20 {
                break;
            }
            i += 1;
        }
        out.extend_from_slice(&input[start..i]);
        match input.get(i) {
            None => return Err(("the string has no closing quote".to_string(), i)),
            Some(b'"') => break,
            Some(b'\\') => {
                let Some(&escape) = input.get(i + 1) else {
                    return Err(("the string has no closing quote".to_string(), i));
                };
                i += 2;
                let ch = match escape {
                    b'"' => '"',
                    b'\\' => '\\',
                    b'/' => '/',
                    b'b' => '\u{8}',
                    b'f' => '\u{c}',
                    b'n' => '\n',
                    b'r' => '\r',
                    b't' => '\t',
                    b'u' => {
                        let Some(unit) = hex4(input, i) else {
                            return Err(("`\\u` needs four hexadecimal digits".to_string(), i));
                        };
                        i += 4;
                        let code = if (0xD800..0xDC00).contains(&unit) {
                            // A high surrogate must be followed by `\u` and a low surrogate.
                            let low = (input.get(i) == Some(&b'\\') && input.get(i + 1) == Some(&b'u'))
                                .then(|| hex4(input, i + 2))
                                .flatten()
                                .filter(|low| (0xDC00..0xE000).contains(low));
                            let Some(low) = low else {
                                return Err(("an unpaired surrogate in a `\\u` escape".to_string(), i));
                            };
                            i += 6;
                            0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00)
                        } else if (0xDC00..0xE000).contains(&unit) {
                            return Err(("an unpaired surrogate in a `\\u` escape".to_string(), i));
                        } else {
                            unit
                        };
                        char::from_u32(code).expect("a valid scalar value")
                    }
                    _ => return Err((format!("invalid escape `\\{}`", char::from(escape)), i - 1)),
                };
                let mut buffer = [0u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut buffer).as_bytes());
            }
            Some(_) => return Err(("a control character must be escaped in a string".to_string(), i)),
        }
    }
    Ok((Text::Decoded(out), i + 1))
}

enum Number {
    Int(i64),
    Float(f64),
}

/// A number starting at `pos`, following JSON's grammar exactly.
fn scan_number(input: &[u8], pos: usize) -> Result<(Number, usize), String> {
    let mut i = pos;
    let negative = input.get(i) == Some(&b'-');
    if negative {
        i += 1;
    }
    // The magnitude of the integer part, while it fits; `None` once it overflows.
    let mut magnitude = Some(0u64);
    match input.get(i) {
        Some(b'0') => {
            i += 1;
            if input.get(i).is_some_and(u8::is_ascii_digit) {
                return Err("a number cannot have a leading zero".to_string());
            }
        }
        Some(b'1'..=b'9') => {
            while let Some(&c @ b'0'..=b'9') = input.get(i) {
                magnitude = magnitude.and_then(|m| m.checked_mul(10)).and_then(|m| m.checked_add(u64::from(c - b'0')));
                i += 1;
            }
        }
        _ => return Err("invalid number".to_string()),
    }
    let mut integral = true;
    if input.get(i) == Some(&b'.') {
        integral = false;
        i += 1;
        if !input.get(i).is_some_and(u8::is_ascii_digit) {
            return Err("a number needs digits after its decimal point".to_string());
        }
        while input.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
    }
    if let Some(b'e' | b'E') = input.get(i) {
        integral = false;
        i += 1;
        if let Some(b'+' | b'-') = input.get(i) {
            i += 1;
        }
        if !input.get(i).is_some_and(u8::is_ascii_digit) {
            return Err("a number needs digits in its exponent".to_string());
        }
        while input.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
    }
    if integral && let Some(m) = magnitude {
        // As `i64`'s parser would: -2^63 fits, 2^63 does not (it becomes a float below).
        if negative && m <= 1 << 63 {
            return Ok((Number::Int(0u64.wrapping_sub(m) as i64), i));
        }
        if !negative && m <= i64::MAX as u64 {
            return Ok((Number::Int(m as i64), i));
        }
    }
    let text = std::str::from_utf8(&input[pos..i]).expect("ASCII");
    let value: f64 = text.parse().map_err(|_| "invalid number".to_string())?;
    if value.is_infinite() {
        return Err("the number is too large".to_string());
    }
    Ok((Number::Float(value), i))
}

/// Skip one value at `pos`, checking its syntax. Returns the position after it.
fn skip(input: &[u8], pos: usize, depth: usize) -> Result<usize, (String, usize)> {
    if depth > MAX_DEPTH {
        return Err(("the JSON is nested too deeply".to_string(), pos));
    }
    let pos = skip_ws(input, pos);
    match input.get(pos) {
        Some(b'"') => scan_string(input, pos).map(|(_, end)| end),
        Some(b'-' | b'0'..=b'9') => scan_number(input, pos).map(|(_, end)| end).map_err(|m| (m, pos)),
        Some(b't') if input[pos..].starts_with(b"true") => Ok(pos + 4),
        Some(b'f') if input[pos..].starts_with(b"false") => Ok(pos + 5),
        Some(b'n') if input[pos..].starts_with(b"null") => Ok(pos + 4),
        Some(&open @ (b'[' | b'{')) => {
            let close = if open == b'[' { b']' } else { b'}' };
            let mut i = skip_ws(input, pos + 1);
            if input.get(i) == Some(&close) {
                return Ok(i + 1);
            }
            loop {
                if open == b'{' {
                    if input.get(i) != Some(&b'"') {
                        return Err(("expected a string key".to_string(), i));
                    }
                    i = scan_string(input, i)?.1;
                    i = skip_ws(input, i);
                    if input.get(i) != Some(&b':') {
                        return Err(("expected `:` after the key".to_string(), i));
                    }
                    i += 1;
                }
                i = skip(input, i, depth + 1)?;
                i = skip_ws(input, i);
                match input.get(i) {
                    Some(&c) if c == close => return Ok(i + 1),
                    Some(b',') => i = skip_ws(input, i + 1),
                    _ => return Err((format!("expected `,` or `{}`", char::from(close)), i)),
                }
            }
        }
        None => Err(("unexpected end of the input".to_string(), pos)),
        Some(_) => Err(("invalid JSON value".to_string(), pos)),
    }
}

// ---- the C interface used by generated decoders -----------------------------------------

/// # Safety
/// `r` comes from `lrt_json_reader_init` and has not been freed.
unsafe fn reader<'a>(r: *mut Reader) -> &'a mut Reader {
    unsafe { &mut *r }
}

/// Set up a reader in the `READER_BYTES` bytes at `r`, over the text `ptr`, `len`.
///
/// # Safety
/// `r` is writable, 8-aligned and `READER_BYTES` long; `ptr` and `len` describe a UTF-8 string
/// that outlives the reader.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_reader_init(r: *mut Reader, ptr: *const u8, len: i64) {
    let input: &'static [u8] = if len <= 0 { &[] } else { unsafe { std::slice::from_raw_parts(ptr, len as usize) } };
    let mut fresh = Reader::new(input);
    fresh.lenient = LENIENT.with(|flag| flag.replace(false));
    // SAFETY: the caller reserved `READER_BYTES` bytes, 8-aligned.
    unsafe { r.write(fresh) };
}

/// Make the next decode on this thread lenient (the `config` module).
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_json_set_lenient(on: i64) {
    LENIENT.with(|flag| flag.set(on != 0));
}

/// How many problems the last lenient decode on this thread found.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_json_problem_count() -> i64 {
    PROBLEMS.with_borrow(|problems| problems.len() as i64)
}

/// Problem `i` of the last lenient decode: its path (`part` 0) or message (1).
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_json_problem(i: i64, part: i64) -> Str {
    PROBLEMS.with_borrow(|problems| {
        let text = usize::try_from(i)
            .ok()
            .and_then(|i| problems.get(i))
            .map_or("", |(path, message)| if part == 0 { path.as_str() } else { message.as_str() });
        Str::from_bytes(text.as_bytes())
    })
}

/// In lenient mode, after the value of the top-level field whose key was pushed at path
/// `index` 0 failed to decode: keep the error as a problem, skip the value, and return 1 so
/// that the decoder goes on with the next field. 0 otherwise, and the decode fails as usual.
///
/// # Safety
/// `r` comes from `lrt_json_reader_init`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_recover(r: *mut Reader, index: i64) -> i64 {
    let r = unsafe { reader(r) };
    if !r.lenient || index != 0 || r.path_len == 0 {
        return 0;
    }
    let Some(error) = r.error.take() else { return 0 };
    match skip(r.input, r.top_start, r.top_depth) {
        Ok(end) => {
            let (message, path) = *error;
            r.problems.push((path, message));
            r.pos = end;
            r.depth = r.top_depth;
            r.path_len = 1;
            r.deeper_path.clear();
            1
        }
        Err(_) => {
            r.error = Some(error);
            0
        }
    }
}

/// A required field `ptr`, `len` is missing from the object just read. In lenient mode, at the
/// top level, it is recorded as a problem and 1 is returned; otherwise it is an error.
///
/// # Safety
/// `r` comes from `lrt_json_reader_init`; `ptr` and `len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_missing(r: *mut Reader, ptr: *const u8, len: i64) -> i64 {
    let name = unsafe { Str::from_raw(ptr, len) }.to_string();
    let r = unsafe { reader(r) };
    if r.lenient && r.path_len == 0 && !r.failed() {
        r.problems.push((name, "is required".to_string()));
        return 1;
    }
    r.fail(format!("missing key \"{name}\""));
    0
}

/// Release what the reader allocated (an error, a deep path). `_input` is the text it read,
/// passed so that generated code keeps it in a scanned register or slot until now, whatever the
/// optimiser does with the reader's own copy of the pointer.
///
/// # Safety
/// `r` was set up by `lrt_json_reader_init` and is not used again.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_reader_free(r: *mut Reader, _input: *const u8) {
    unsafe { std::ptr::drop_in_place(r) };
}

/// After the value: only whitespace may follow. Returns 1 if the whole decode succeeded.
///
/// # Safety
/// `r` comes from `lrt_json_reader_init`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_finish(r: *mut Reader) -> i64 {
    let r = unsafe { reader(r) };
    if !r.failed() {
        r.skip_ws();
        if r.pos < r.input.len() {
            r.clear_path();
            r.fail("unexpected text after the JSON value");
        }
    }
    if r.lenient {
        let mut problems = std::mem::take(&mut r.problems);
        if let Some(error) = &r.error {
            problems.push((error.1.clone(), error.0.clone()));
        } else if let Some((path, message)) = problems.first() {
            r.error = Some(Box::new((message.clone(), path.clone())));
        }
        PROBLEMS.with_borrow_mut(|stash| *stash = problems);
    }
    i64::from(!r.failed())
}

/// # Safety
/// `r` comes from `lrt_json_reader_init`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_failed(r: *mut Reader) -> i64 {
    i64::from(unsafe { reader(r) }.failed())
}

/// # Safety
/// `r` comes from `lrt_json_reader_init`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_error_message(r: *mut Reader) -> Str {
    Str::from_bytes(unsafe { reader(r) }.error.as_ref().map_or("", |e| e.0.as_str()).as_bytes())
}

/// # Safety
/// `r` comes from `lrt_json_reader_init`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_error_path(r: *mut Reader) -> Str {
    Str::from_bytes(unsafe { reader(r) }.error.as_ref().map_or("", |e| e.1.as_str()).as_bytes())
}

/// Enter the value of the key `ptr`, `len`, for error paths. Only the pointer is kept.
///
/// # Safety
/// `r` comes from `lrt_json_reader_init`; `ptr` and `len` describe a UTF-8 string that stays
/// alive until the matching `lrt_json_pop`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_push_key(r: *mut Reader, ptr: *const u8, len: i64) -> i64 {
    // A key is never null: an empty one still points somewhere.
    let ptr = if ptr.is_null() { b"".as_ptr() } else { ptr };
    let r = unsafe { reader(r) };
    let index = r.path_len as i64;
    r.push_path(PathPart { ptr, len });
    index
}

/// # Safety
/// `r` comes from `lrt_json_reader_init`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_push_index(r: *mut Reader, index: i64) {
    unsafe { reader(r) }.push_path(PathPart { ptr: std::ptr::null(), len: index });
}

/// # Safety
/// `r` comes from `lrt_json_reader_init`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_pop(r: *mut Reader) {
    unsafe { reader(r) }.pop_path();
}

/// Record that something else was expected here (`what` is "an integer", ...).
///
/// # Safety
/// `r` comes from `lrt_json_reader_init`; `ptr` and `len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_fail_expected(r: *mut Reader, ptr: *const u8, len: i64) {
    let what = unsafe { Str::from_raw(ptr, len) }.to_string();
    let r = unsafe { reader(r) };
    r.skip_ws();
    r.expected(&what);
}

/// Record an error message.
///
/// # Safety
/// `r` comes from `lrt_json_reader_init`; `ptr` and `len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_fail(r: *mut Reader, ptr: *const u8, len: i64) {
    let message = unsafe { Str::from_raw(ptr, len) }.to_string();
    unsafe { reader(r) }.fail(message);
}

/// 1 if `null` is next, which is consumed.
///
/// # Safety
/// `r` comes from `lrt_json_reader_init`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_read_null(r: *mut Reader) -> i64 {
    let r = unsafe { reader(r) };
    if r.failed() {
        return 0;
    }
    r.skip_ws();
    i64::from(r.literal(b"null"))
}

/// 1 for `true`, 0 for `false`, -1 (after recording an error) for anything else.
///
/// # Safety
/// `r` comes from `lrt_json_reader_init`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_read_bool(r: *mut Reader) -> i64 {
    let r = unsafe { reader(r) };
    if r.failed() {
        return -1;
    }
    if let Some(text) = r.lenient_text() {
        return match lenient_bool(&text) {
            Some(value) => i64::from(value),
            None => {
                r.fail(format!("must be true or false, got {}", shown(&text)));
                -1
            }
        };
    }
    r.skip_ws();
    if r.literal(b"true") {
        1
    } else if r.literal(b"false") {
        0
    } else {
        r.expected("a boolean");
        -1
    }
}

fn number(r: &mut Reader) -> Option<Number> {
    if r.failed() {
        return None;
    }
    r.skip_ws();
    if !matches!(r.peek(), Some(b'-' | b'0'..=b'9')) {
        return None;
    }
    match scan_number(r.input, r.pos) {
        Ok((number, end)) => {
            r.pos = end;
            Some(number)
        }
        Err(message) => {
            r.fail(message);
            None
        }
    }
}

/// An integer into `out`; 1 on success.
///
/// # Safety
/// `r` comes from `lrt_json_reader_init`; `out` is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_read_int(r: *mut Reader, out: *mut i64) -> i64 {
    let r = unsafe { reader(r) };
    if let Some(text) = r.lenient_text() {
        return match text.parse::<i64>() {
            Ok(value) => {
                unsafe { *out = value };
                1
            }
            Err(_) => {
                r.fail(format!("must be an integer, got {}", shown(&text)));
                0
            }
        };
    }
    let start = {
        r.skip_ws();
        r.pos
    };
    match number(r) {
        Some(Number::Int(value)) => {
            unsafe { *out = value };
            1
        }
        Some(Number::Float(_)) => {
            let text = String::from_utf8_lossy(&r.input[start..r.pos]).into_owned();
            r.fail(format!("expected an integer, found {text}"));
            0
        }
        None => {
            if !r.failed() {
                r.expected("an integer");
            }
            0
        }
    }
}

/// A number into `out` as a float; 1 on success.
///
/// # Safety
/// `r` comes from `lrt_json_reader_init`; `out` is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_read_float(r: *mut Reader, out: *mut f64) -> i64 {
    let r = unsafe { reader(r) };
    if let Some(text) = r.lenient_text() {
        return match text.parse::<f64>() {
            Ok(value) if value.is_finite() => {
                unsafe { *out = value };
                1
            }
            _ => {
                r.fail(format!("must be a number, got {}", shown(&text)));
                0
            }
        };
    }
    match number(r) {
        Some(Number::Int(value)) => {
            unsafe { *out = value as f64 };
            1
        }
        Some(Number::Float(value)) => {
            unsafe { *out = value };
            1
        }
        None => {
            if !r.failed() {
                r.expected("a number");
            }
            0
        }
    }
}

/// A number: 1 with an integer in `int_out`, 2 with a float in `float_out`, 0 on error.
///
/// # Safety
/// `r` comes from `lrt_json_reader_init`; the outputs are writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_read_number(r: *mut Reader, int_out: *mut i64, float_out: *mut f64) -> i64 {
    let r = unsafe { reader(r) };
    match number(r) {
        Some(Number::Int(value)) => {
            unsafe { *int_out = value };
            1
        }
        Some(Number::Float(value)) => {
            unsafe { *float_out = value };
            2
        }
        None => {
            if !r.failed() {
                r.expected("a number");
            }
            0
        }
    }
}

/// A string, or a null `Str` after recording an error.
///
/// # Safety
/// `r` comes from `lrt_json_reader_init`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_read_str(r: *mut Reader) -> Str {
    let r = unsafe { reader(r) };
    let null = Str { ptr: std::ptr::null(), len: 0 };
    if r.failed() {
        return null;
    }
    match r.string() {
        Some(text) => text.to_str(),
        None => null,
    }
}

/// Start an array; 1 on success.
///
/// # Safety
/// `r` comes from `lrt_json_reader_init`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_array_start(r: *mut Reader) -> i64 {
    i64::from(unsafe { reader(r) }.open(b'[', "an array"))
}

/// 1 if another element follows, 0 at the end of the array, -1 on error.
///
/// # Safety
/// `r` comes from `lrt_json_reader_init`, inside an array.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_array_next(r: *mut Reader) -> i64 {
    unsafe { reader(r) }.next(b']')
}

/// Start an object; 1 on success.
///
/// # Safety
/// `r` comes from `lrt_json_reader_init`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_object_start(r: *mut Reader) -> i64 {
    i64::from(unsafe { reader(r) }.open(b'{', "an object"))
}

/// The next key of the open object, with its `:` consumed; a null `Str` at the end of the
/// object or on error (check `lrt_json_failed`).
///
/// # Safety
/// `r` comes from `lrt_json_reader_init`, inside an object.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_object_key(r: *mut Reader) -> Str {
    match unsafe { reader(r) }.key() {
        Some(key) => key.to_str(),
        None => Str { ptr: std::ptr::null(), len: 0 },
    }
}

/// Like `lrt_json_object_key`, but a key without escapes is returned as a view into the input
/// instead of a copy: for keys that are only compared, never stored. Generated code keeps the
/// input alive (it passes it to `lrt_json_reader_free`), and the collector treats a pointer into
/// the middle of a string as a reference to it.
///
/// # Safety
/// `r` comes from `lrt_json_reader_init`, inside an object.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_object_key_view(r: *mut Reader) -> Str {
    match unsafe { reader(r) }.key() {
        // An empty key must not be a null `Str`, which means the end of the object.
        Some(Text::Raw(bytes)) => Str { ptr: bytes.as_ptr(), len: bytes.len() as i64 },
        Some(key) => key.to_str(),
        None => Str { ptr: std::ptr::null(), len: 0 },
    }
}

/// Skip a value, checking its syntax; 1 on success.
///
/// # Safety
/// `r` comes from `lrt_json_reader_init`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_skip_value(r: *mut Reader) -> i64 {
    let r = unsafe { reader(r) };
    if r.failed() {
        return 0;
    }
    match skip(r.input, r.pos, r.depth) {
        Ok(end) => {
            r.pos = end;
            1
        }
        Err((message, at)) => {
            r.pos = at;
            r.fail(message);
            0
        }
    }
}

/// What comes next: 0 null, 1 boolean, 2 number, 3 string, 4 array, 5 object; -1 (after
/// recording an error) for anything else.
///
/// # Safety
/// `r` comes from `lrt_json_reader_init`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_peek_kind(r: *mut Reader) -> i64 {
    let r = unsafe { reader(r) };
    if r.failed() {
        return -1;
    }
    r.skip_ws();
    match r.peek() {
        Some(b'n') => 0,
        Some(b't' | b'f') => 1,
        Some(b'-' | b'0'..=b'9') => 2,
        Some(b'"') => 3,
        Some(b'[') => 4,
        Some(b'{') => 5,
        _ => {
            r.expected("a JSON value");
            -1
        }
    }
}

/// The string value of the key `key` in the object that starts here, without consuming
/// anything: how a tagged enum finds its variant before reading its fields. A null `Str` if the
/// object has no such key, or its value is not a string.
///
/// # Safety
/// `r` comes from `lrt_json_reader_init`; `ptr` and `len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_peek_tag(r: *mut Reader, ptr: *const u8, len: i64) -> Str {
    let wanted = unsafe { Str::from_raw(ptr, len) };
    let r = unsafe { reader(r) };
    let null = Str { ptr: std::ptr::null(), len: 0 };
    let input = r.input;
    let mut i = skip_ws(input, r.pos);
    if input.get(i) != Some(&b'{') {
        return null;
    }
    i = skip_ws(input, i + 1);
    while input.get(i) == Some(&b'"') {
        let Ok((key, end)) = scan_string(input, i) else { return null };
        i = skip_ws(input, end);
        if input.get(i) != Some(&b':') {
            return null;
        }
        i = skip_ws(input, i + 1);
        if key.bytes() == wanted.as_bytes() {
            return match scan_string(input, i) {
                Ok((value, _)) if input.get(i) == Some(&b'"') => value.to_str(),
                _ => null,
            };
        }
        let Ok(end) = skip(input, i, r.depth + 1) else { return null };
        i = skip_ws(input, end);
        if input.get(i) != Some(&b',') {
            return null;
        }
        i = skip_ws(input, i + 1);
    }
    null
}

// ---- writing ------------------------------------------------------------------------------

/// Append a string as a JSON string literal: quoted, with `"`, `\` and control characters
/// escaped. Other characters, including non-ASCII ones, are written as they are.
///
/// # Safety
/// `b` comes from `lrt_builder_new`; `ptr` and `len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_builder_push_json_str(b: *mut Builder, ptr: *const u8, len: i64) {
    let text = unsafe { Str::from_raw(ptr, len) }.as_bytes();
    let b = unsafe { &mut *b };
    b.reserve(text.len() + 2);
    push_json_string(b, text);
}

/// Somewhere JSON text is written: a `Vec` or, without a copy in between, a builder.
pub(crate) trait JsonSink {
    fn put(&mut self, bytes: &[u8]);
}

impl JsonSink for Vec<u8> {
    fn put(&mut self, bytes: &[u8]) {
        self.extend_from_slice(bytes);
    }
}

impl JsonSink for Builder {
    fn put(&mut self, bytes: &[u8]) {
        self.push(bytes);
    }
}

/// Whether a byte must be escaped inside a JSON string.
fn needs_escape(c: u8) -> bool {
    c == b'"' || c == b'\\' || c < 0x20
}

/// The index of the first byte at or after `i` that must be escaped, or the length of `text`.
/// Looks at eight bytes at a time: most strings have nothing to escape.
fn next_escape(text: &[u8], mut i: usize) -> usize {
    const ONES: u64 = 0x0101_0101_0101_0101;
    const HIGH: u64 = 0x8080_8080_8080_8080;
    loop {
        while let Some(chunk) = text.get(i..i + 8) {
            let word = u64::from_le_bytes(chunk.try_into().unwrap());
            let quote = word ^ (ONES * u64::from(b'"'));
            let backslash = word ^ (ONES * u64::from(b'\\'));
            // A byte's high bit is set in each term when that byte is zero (or below 0x20).
            let found = (quote.wrapping_sub(ONES) & !quote)
                | (backslash.wrapping_sub(ONES) & !backslash)
                | (word.wrapping_sub(ONES * 0x20) & !word);
            if found & HIGH != 0 {
                break;
            }
            i += 8;
        }
        let end = (i + 8).min(text.len());
        while i < end {
            if needs_escape(text[i]) {
                return i;
            }
            i += 1;
        }
        if i >= text.len() {
            return text.len();
        }
    }
}

/// Append UTF-8 text as a JSON string literal.
pub(crate) fn push_json_string(out: &mut impl JsonSink, text: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.put(b"\"");
    let mut start = 0;
    loop {
        let i = next_escape(text, start);
        out.put(&text[start..i]);
        let Some(&c) = text.get(i) else { break };
        match c {
            b'"' => out.put(b"\\\""),
            b'\\' => out.put(b"\\\\"),
            b'\n' => out.put(b"\\n"),
            b'\r' => out.put(b"\\r"),
            b'\t' => out.put(b"\\t"),
            0x08 => out.put(b"\\b"),
            0x0c => out.put(b"\\f"),
            _ => out.put(&[b'\\', b'u', b'0', b'0', HEX[usize::from(c >> 4)], HEX[usize::from(c & 15)]]),
        }
        start = i + 1;
    }
    out.put(b"\"");
}

/// Append a float as a JSON number, the way Python writes it (`1.0`, `1e+100`). NaN and the
/// infinities have no JSON form and are written as `null`.
///
/// # Safety
/// `b` comes from `lrt_builder_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_builder_push_json_float(b: *mut Builder, value: f64) {
    let mut text = [0; lang_format::FLOAT_REPR_MAX];
    let text: &[u8] = if value.is_finite() {
        let len = lang_format::write_float_repr(value, &mut text);
        &text[..len]
    } else {
        b"null"
    };
    unsafe { lrt_builder_push(b, text.as_ptr(), text.len() as i64) };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The byte-at-a-time escaping the eight-at-a-time scan must agree with.
    fn simple(text: &[u8]) -> Vec<u8> {
        let mut out = vec![b'"'];
        for &c in text {
            match c {
                b'"' => out.extend_from_slice(b"\\\""),
                b'\\' => out.extend_from_slice(b"\\\\"),
                b'\n' => out.extend_from_slice(b"\\n"),
                b'\r' => out.extend_from_slice(b"\\r"),
                b'\t' => out.extend_from_slice(b"\\t"),
                0x08 => out.extend_from_slice(b"\\b"),
                0x0c => out.extend_from_slice(b"\\f"),
                c if c < 0x20 => out.extend_from_slice(format!("\\u{c:04x}").as_bytes()),
                c => out.push(c),
            }
        }
        out.push(b'"');
        out
    }

    #[test]
    fn strings_escape_like_the_simple_loop() {
        let alphabet: &[u8] = b"ab\"\\\n\r\t\x08\x0c\x00\x1f\x7f \xc3\xa9~\x21\x5b\x5d";
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        for len in 0..80 {
            for _ in 0..200 {
                let text: Vec<u8> = (0..len)
                    .map(|_| {
                        state ^= state << 13;
                        state ^= state >> 7;
                        state ^= state << 17;
                        // Mostly plain letters, so that long runs without escapes occur.
                        if state % 4 == 0 {
                            alphabet[(state >> 8) as usize % alphabet.len()]
                        } else {
                            b'a' + (state >> 16) as u8 % 26
                        }
                    })
                    .collect();
                let mut out = Vec::new();
                push_json_string(&mut out, &text);
                assert_eq!(out, simple(&text), "{text:?}");
            }
        }
        for c in 0..=255u8 {
            let text = [b'x', b'x', b'x', b'x', b'x', b'x', b'x', b'x', c, b'y'];
            let mut out = Vec::new();
            push_json_string(&mut out, &text);
            assert_eq!(out, simple(&text), "byte {c:#x}");
        }
    }

    /// A lenient reader over `text`, as `config.load` sets one up.
    fn lenient(text: &str) -> Reader {
        let mut r = Reader::new(Box::leak(text.as_bytes().to_vec().into_boxed_slice()));
        r.lenient = true;
        r
    }

    fn message(r: &Reader) -> String {
        r.error.as_ref().map_or(String::new(), |e| e.0.clone())
    }

    #[test]
    fn lenient_scalars_parse_their_text() {
        for (text, expected) in [("\" YES \"", 1), ("\"no\"", 0), ("\"1\"", 1), ("\"False\"", 0), ("true", 1)] {
            let mut r = lenient(text);
            assert_eq!(unsafe { lrt_json_read_bool(&mut r) }, expected, "{text}");
        }
        let mut r = lenient("\"maybe\"");
        assert_eq!(unsafe { lrt_json_read_bool(&mut r) }, -1);
        assert_eq!(message(&r), "must be true or false, got \"maybe\"");

        let mut out = 0i64;
        let mut r = lenient("\" 8080 \"");
        assert_eq!(unsafe { lrt_json_read_int(&mut r, &mut out) }, 1);
        assert_eq!(out, 8080);
        let mut r = lenient("\"80a\"");
        assert_eq!(unsafe { lrt_json_read_int(&mut r, &mut out) }, 0);
        assert_eq!(message(&r), "must be an integer, got \"80a\"");
        let mut r = lenient("\"99999999999999999999\"");
        assert_eq!(unsafe { lrt_json_read_int(&mut r, &mut out) }, 0);
        assert_eq!(message(&r), "must be an integer, got \"99999999999999999999\"");

        let mut float = 0f64;
        let mut r = lenient("\"0.25\"");
        assert_eq!(unsafe { lrt_json_read_float(&mut r, &mut float) }, 1);
        assert_eq!(float, 0.25);
        let mut r = lenient("\"inf\"");
        assert_eq!(unsafe { lrt_json_read_float(&mut r, &mut float) }, 0);
        assert_eq!(message(&r), "must be a number, got \"inf\"");

        // Strict readers still want JSON.
        let mut r = lenient("\"1\"");
        r.lenient = false;
        assert_eq!(unsafe { lrt_json_read_int(&mut r, &mut out) }, 0);

        // Long values are cut short in the message.
        let long = format!("\"{}\"", "x".repeat(60));
        let mut r = lenient(&long);
        assert_eq!(unsafe { lrt_json_read_bool(&mut r) }, -1);
        assert_eq!(message(&r), format!("must be true or false, got \"{}\"...", "x".repeat(40)));
    }

    /// The strings of the array the reader is at.
    fn strings(r: &mut Reader) -> Result<Vec<String>, String> {
        let mut items = Vec::new();
        if unsafe { lrt_json_array_start(r) } != 1 {
            return Err(message(r));
        }
        while unsafe { lrt_json_array_next(r) } == 1 {
            match r.string() {
                Some(text) => items.push(String::from_utf8_lossy(text.bytes()).into_owned()),
                None => return Err(message(r)),
            }
        }
        if r.failed() { Err(message(r)) } else { Ok(items) }
    }

    #[test]
    fn lenient_arrays_and_objects() {
        assert_eq!(strings(&mut lenient("\"a, b ,,c\"")), Ok(vec!["a".into(), "b".into(), "c".into()]));
        assert_eq!(strings(&mut lenient("\" [\\\"a\\\", \\\"b,c\\\"] \"")), Ok(vec!["a".into(), "b,c".into()]));
        assert_eq!(strings(&mut lenient("\"\"")), Ok(vec![]));
        assert_eq!(strings(&mut lenient("\"[1\"")), Err("is not valid JSON: expected `,` or `]`".into()));
        assert_eq!(strings(&mut lenient("\"[] x\"")), Err("is not valid JSON: unexpected text at position 4".into()));
        let mut r = lenient("\"lots\"");
        assert_eq!(unsafe { lrt_json_object_start(&mut r) }, 0);
        assert_eq!(message(&r), "must be a JSON object, got \"lots\"");
    }
}
