//! The JSON scanner and writer (docs/05-standard-library.md, "JSON").
//!
//! Generated decoders drive a [`Reader`] one token at a time: they ask for a number, a string,
//! the next element of an array or the next key of an object, and the reader checks the syntax
//! as it goes (RFC 8259, strictly: no comments, no trailing commas, no single quotes, no leading
//! zeros, no lone surrogates). The first error is kept, with the path to where it happened
//! (`addresses[1].zip`), and every later call fails quickly.
//!
//! The reader lives on Rust's heap and holds no references to collected objects; the input text
//! is kept alive by the caller for the duration of the decode.

use crate::Str;
use crate::builder::{Builder, lrt_builder_push};

/// Nesting deeper than this is an error, so that hostile input cannot exhaust the stack.
const MAX_DEPTH: usize = 512;

enum PathPart {
    Key(String),
    Index(i64),
}

pub struct Reader {
    input: &'static [u8],
    pos: usize,
    /// For each open array or object: whether no element has been read yet.
    frames: Vec<bool>,
    path: Vec<PathPart>,
    error: Option<(String, String)>,
}

impl Reader {
    fn fail(&mut self, message: impl Into<String>) {
        if self.error.is_none() {
            let mut path = String::new();
            for part in &self.path {
                match part {
                    PathPart::Key(key) if path.is_empty() => path.push_str(key),
                    PathPart::Key(key) => {
                        path.push('.');
                        path.push_str(key);
                    }
                    PathPart::Index(i) => path.push_str(&format!("[{i}]")),
                }
            }
            self.error = Some((message.into(), path));
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

    fn string(&mut self) -> Option<String> {
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

    fn open(&mut self, open: u8, what: &str) -> bool {
        if self.failed() {
            return false;
        }
        self.skip_ws();
        if self.peek() != Some(open) {
            self.expected(what);
            return false;
        }
        if self.frames.len() >= MAX_DEPTH {
            self.fail("the JSON is nested too deeply");
            return false;
        }
        self.pos += 1;
        self.frames.push(true);
        true
    }

    /// Before the next element of the open array or object: 1 if there is one, 0 at its end.
    fn next(&mut self, close: u8) -> i64 {
        if self.failed() {
            return -1;
        }
        self.skip_ws();
        let first = *self.frames.last().expect("inside an array or object");
        if self.peek() == Some(close) {
            self.pos += 1;
            self.frames.pop();
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
        *self.frames.last_mut().expect("inside an array or object") = false;
        1
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

/// A string starting at the `"` at `pos`: its text and the position after its closing quote, or
/// an error message and where.
fn scan_string(input: &[u8], pos: usize) -> Result<(String, usize), (String, usize)> {
    let mut i = pos + 1;
    let mut out: Vec<u8> = Vec::new();
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
    // The input is valid UTF-8 and escapes produce valid UTF-8.
    Ok((String::from_utf8(out).expect("valid UTF-8"), i + 1))
}

enum Number {
    Int(i64),
    Float(f64),
}

/// A number starting at `pos`, following JSON's grammar exactly.
fn scan_number(input: &[u8], pos: usize) -> Result<(Number, usize), String> {
    let mut i = pos;
    if input.get(i) == Some(&b'-') {
        i += 1;
    }
    match input.get(i) {
        Some(b'0') => {
            i += 1;
            if input.get(i).is_some_and(u8::is_ascii_digit) {
                return Err("a number cannot have a leading zero".to_string());
            }
        }
        Some(b'1'..=b'9') => {
            while input.get(i).is_some_and(u8::is_ascii_digit) {
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
    let text = std::str::from_utf8(&input[pos..i]).expect("ASCII");
    if integral && let Ok(value) = text.parse::<i64>() {
        return Ok((Number::Int(value), i));
    }
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
/// `r` comes from `lrt_json_reader_new` and has not been freed.
unsafe fn reader<'a>(r: *mut Reader) -> &'a mut Reader {
    unsafe { &mut *r }
}

/// # Safety
/// `ptr` and `len` describe a UTF-8 string that outlives the reader.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_reader_new(ptr: *const u8, len: i64) -> *mut Reader {
    let input: &'static [u8] = if len <= 0 { &[] } else { unsafe { std::slice::from_raw_parts(ptr, len as usize) } };
    Box::into_raw(Box::new(Reader { input, pos: 0, frames: Vec::new(), path: Vec::new(), error: None }))
}

/// # Safety
/// `r` comes from `lrt_json_reader_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_reader_free(r: *mut Reader) {
    drop(unsafe { Box::from_raw(r) });
}

/// After the value: only whitespace may follow. Returns 1 if the whole decode succeeded.
///
/// # Safety
/// `r` comes from `lrt_json_reader_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_finish(r: *mut Reader) -> i64 {
    let r = unsafe { reader(r) };
    if !r.failed() {
        r.skip_ws();
        if r.pos < r.input.len() {
            r.path.clear();
            r.fail("unexpected text after the JSON value");
        }
    }
    i64::from(!r.failed())
}

/// # Safety
/// `r` comes from `lrt_json_reader_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_failed(r: *mut Reader) -> i64 {
    i64::from(unsafe { reader(r) }.failed())
}

/// # Safety
/// `r` comes from `lrt_json_reader_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_error_message(r: *mut Reader) -> Str {
    let message = unsafe { reader(r) }.error.as_ref().map_or("", |(m, _)| m.as_str()).to_string();
    Str::from_bytes(message.as_bytes())
}

/// # Safety
/// `r` comes from `lrt_json_reader_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_error_path(r: *mut Reader) -> Str {
    let path = unsafe { reader(r) }.error.as_ref().map_or("", |(_, p)| p.as_str()).to_string();
    Str::from_bytes(path.as_bytes())
}

/// # Safety
/// `r` comes from `lrt_json_reader_new`; `ptr` and `len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_push_key(r: *mut Reader, ptr: *const u8, len: i64) {
    let key = unsafe { Str::from_raw(ptr, len) }.to_string();
    unsafe { reader(r) }.path.push(PathPart::Key(key));
}

/// # Safety
/// `r` comes from `lrt_json_reader_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_push_index(r: *mut Reader, index: i64) {
    unsafe { reader(r) }.path.push(PathPart::Index(index));
}

/// # Safety
/// `r` comes from `lrt_json_reader_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_pop(r: *mut Reader) {
    unsafe { reader(r) }.path.pop();
}

/// Record that something else was expected here (`what` is "an integer", ...).
///
/// # Safety
/// `r` comes from `lrt_json_reader_new`; `ptr` and `len` describe a UTF-8 string.
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
/// `r` comes from `lrt_json_reader_new`; `ptr` and `len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_fail(r: *mut Reader, ptr: *const u8, len: i64) {
    let message = unsafe { Str::from_raw(ptr, len) }.to_string();
    unsafe { reader(r) }.fail(message);
}

/// 1 if `null` is next, which is consumed.
///
/// # Safety
/// `r` comes from `lrt_json_reader_new`.
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
/// `r` comes from `lrt_json_reader_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_read_bool(r: *mut Reader) -> i64 {
    let r = unsafe { reader(r) };
    if r.failed() {
        return -1;
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
/// `r` comes from `lrt_json_reader_new`; `out` is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_read_int(r: *mut Reader, out: *mut i64) -> i64 {
    let r = unsafe { reader(r) };
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
/// `r` comes from `lrt_json_reader_new`; `out` is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_read_float(r: *mut Reader, out: *mut f64) -> i64 {
    let r = unsafe { reader(r) };
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
/// `r` comes from `lrt_json_reader_new`; the outputs are writable.
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
/// `r` comes from `lrt_json_reader_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_read_str(r: *mut Reader) -> Str {
    let r = unsafe { reader(r) };
    let null = Str { ptr: std::ptr::null(), len: 0 };
    if r.failed() {
        return null;
    }
    match r.string() {
        Some(text) => Str::from_bytes(text.as_bytes()),
        None => null,
    }
}

/// Start an array; 1 on success.
///
/// # Safety
/// `r` comes from `lrt_json_reader_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_array_start(r: *mut Reader) -> i64 {
    i64::from(unsafe { reader(r) }.open(b'[', "an array"))
}

/// 1 if another element follows, 0 at the end of the array, -1 on error.
///
/// # Safety
/// `r` comes from `lrt_json_reader_new`, inside an array.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_array_next(r: *mut Reader) -> i64 {
    unsafe { reader(r) }.next(b']')
}

/// Start an object; 1 on success.
///
/// # Safety
/// `r` comes from `lrt_json_reader_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_object_start(r: *mut Reader) -> i64 {
    i64::from(unsafe { reader(r) }.open(b'{', "an object"))
}

/// The next key of the open object, with its `:` consumed; a null `Str` at the end of the
/// object or on error (check `lrt_json_failed`).
///
/// # Safety
/// `r` comes from `lrt_json_reader_new`, inside an object.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_object_key(r: *mut Reader) -> Str {
    let r = unsafe { reader(r) };
    let null = Str { ptr: std::ptr::null(), len: 0 };
    if r.next(b'}') != 1 {
        return null;
    }
    let Some(key) = r.string() else { return null };
    r.skip_ws();
    if r.peek() != Some(b':') {
        r.expected("`:` after the key");
        return null;
    }
    r.pos += 1;
    Str::from_bytes(key.as_bytes())
}

/// Skip a value, checking its syntax; 1 on success.
///
/// # Safety
/// `r` comes from `lrt_json_reader_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_json_skip_value(r: *mut Reader) -> i64 {
    let r = unsafe { reader(r) };
    if r.failed() {
        return 0;
    }
    match skip(r.input, r.pos, r.frames.len()) {
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
/// `r` comes from `lrt_json_reader_new`.
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
/// `r` comes from `lrt_json_reader_new`; `ptr` and `len` describe a UTF-8 string.
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
        if key == wanted {
            return match scan_string(input, i) {
                Ok((value, _)) if input.get(i) == Some(&b'"') => Str::from_bytes(value.as_bytes()),
                _ => null,
            };
        }
        let Ok(end) = skip(input, i, r.frames.len() + 1) else { return null };
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
    let mut out = Vec::with_capacity(text.len() + 2);
    push_json_string(&mut out, text);
    unsafe { lrt_builder_push(b, out.as_ptr(), out.len() as i64) };
}

/// Append UTF-8 text to `out` as a JSON string literal.
pub(crate) fn push_json_string(out: &mut Vec<u8>, text: &[u8]) {
    out.push(b'"');
    let mut start = 0;
    for (i, &c) in text.iter().enumerate() {
        let escape: Option<&[u8]> = match c {
            b'"' => Some(b"\\\""),
            b'\\' => Some(b"\\\\"),
            b'\n' => Some(b"\\n"),
            b'\r' => Some(b"\\r"),
            b'\t' => Some(b"\\t"),
            0x08 => Some(b"\\b"),
            0x0c => Some(b"\\f"),
            _ => None,
        };
        if escape.is_some() || c < 0x20 {
            out.extend_from_slice(&text[start..i]);
            match escape {
                Some(e) => out.extend_from_slice(e),
                None => out.extend_from_slice(format!("\\u{c:04x}").as_bytes()),
            }
            start = i + 1;
        }
    }
    out.extend_from_slice(&text[start..]);
    out.push(b'"');
}

/// Append a float as a JSON number, the way Python writes it (`1.0`, `1e+100`). NaN and the
/// infinities have no JSON form and are written as `null`.
///
/// # Safety
/// `b` comes from `lrt_builder_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_builder_push_json_float(b: *mut Builder, value: f64) {
    let text = if value.is_finite() { lang_format::float_repr(value) } else { "null".to_string() };
    unsafe { lrt_builder_push(b, text.as_ptr(), text.len() as i64) };
}
