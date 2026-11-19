//! A growable string buffer in collected memory. Generated code uses it for f-strings, `str(x)`
//! and `print`: create, push pieces, then finish into a `str` or print it.
//!
//! The `Builder` record itself lives on the generated function's stack (zeroed, then optionally
//! given a capacity with `lrt_builder_reserve`); only its buffer is collected memory. The stack
//! is scanned, so the buffer stays alive while the builder is in use.

use crate::{Str, gc};

#[repr(C)]
pub struct Builder {
    data: *mut u8,
    len: i64,
    cap: i64,
}

impl Builder {
    fn grow_to(&mut self, cap: usize) {
        // The collector adds a byte to every object and rounds to 16-byte granules, so a
        // request one byte short of a granule boundary wastes nothing.
        let cap = (cap + 1).next_multiple_of(16) - 1;
        let data = gc::alloc_atomic(cap);
        if self.len > 0 {
            // SAFETY: both regions are at least `len` bytes and do not overlap.
            unsafe { std::ptr::copy_nonoverlapping(self.data, data, self.len as usize) };
        }
        self.data = data;
        self.cap = cap as i64;
    }

    /// Make room for `additional` more bytes, growing as `push` would.
    pub fn reserve(&mut self, additional: usize) {
        let needed = self.len as usize + additional;
        if needed > self.cap as usize {
            self.grow_to(needed.max(self.cap as usize * 2).max(32));
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.reserve(bytes.len());
        // SAFETY: capacity was ensured above.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.data.add(self.len as usize), bytes.len()) };
        self.len += bytes.len() as i64;
    }

    fn bytes(&self) -> &[u8] {
        if self.len == 0 {
            return &[];
        }
        // SAFETY: `data` holds `len` initialised bytes.
        unsafe { std::slice::from_raw_parts(self.data, self.len as usize) }
    }
}

/// Python's `repr` of a string: quoted, with escapes.
pub fn str_repr(text: &str) -> String {
    let mut out = Vec::with_capacity(text.len() + 2);
    write_str_repr(text, |bytes| out.extend_from_slice(bytes));
    // Escapes are ASCII and the rest is copied from valid UTF-8 at character boundaries.
    String::from_utf8(out).expect("valid UTF-8")
}

/// Write Python's `repr` of a string through `put`, copying runs that need no escape whole.
/// Every byte that needs one is ASCII, so scanning bytes never splits a character.
fn write_str_repr(text: &str, mut put: impl FnMut(&[u8])) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let quote = if text.contains('\'') && !text.contains('"') { b'"' } else { b'\'' };
    let bytes = text.as_bytes();
    put(&[quote]);
    let mut start = 0;
    for (i, &c) in bytes.iter().enumerate() {
        let escape: &[u8] = match c {
            b'\\' => b"\\\\",
            b'\n' => b"\\n",
            b'\r' => b"\\r",
            b'\t' => b"\\t",
            c if c == quote => {
                if quote == b'"' {
                    b"\\\""
                } else {
                    b"\\'"
                }
            }
            c if c < 0x20 || c == 0x7f => &[b'\\', b'x', HEX[usize::from(c >> 4)], HEX[usize::from(c & 15)]],
            _ => continue,
        };
        put(&bytes[start..i]);
        put(escape);
        start = i + 1;
    }
    put(&bytes[start..]);
    put(&[quote]);
}

impl std::fmt::Write for Builder {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.push(s.as_bytes());
        Ok(())
    }
}

/// # Safety
/// The pointer must come from `lrt_builder_new`.
unsafe fn builder<'a>(b: *mut Builder) -> &'a mut Builder {
    unsafe { &mut *b }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_builder_new() -> *mut Builder {
    gc::alloc(std::mem::size_of::<Builder>()) as *mut Builder
}

/// Make room for at least `additional` more bytes, so that pieces up to that size are written
/// without growing. Generated code calls this once per f-string with an estimate of its length.
///
/// # Safety
/// `b` points to a builder.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_builder_reserve(b: *mut Builder, additional: i64) {
    let b = unsafe { builder(b) };
    let needed = b.len as usize + additional.max(0) as usize;
    if needed > b.cap as usize {
        b.grow_to(needed);
    }
}

/// The decimal digits of `value`, written into the end of `buffer`; returns where they start.
fn format_u64(mut value: u64, buffer: &mut [u8; 20]) -> usize {
    let mut at = buffer.len();
    loop {
        at -= 1;
        buffer[at] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            return at;
        }
    }
}

fn push_i64(b: &mut Builder, value: i64) {
    let mut buffer = [0u8; 20];
    let at = format_u64(value.unsigned_abs(), &mut buffer);
    if value < 0 {
        b.push(b"-");
    }
    b.push(&buffer[at..]);
}

/// # Safety
/// `b` comes from `lrt_builder_new`; `ptr` and `len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_builder_push(b: *mut Builder, ptr: *const u8, len: i64) {
    unsafe { builder(b).push(Str::from_raw(ptr, len).as_bytes()) };
}

/// # Safety
/// `b` comes from `lrt_builder_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_builder_push_int(b: *mut Builder, value: i64) {
    push_i64(unsafe { builder(b) }, value);
}

/// Append an unsigned 64-bit integer (a `u64`).
///
/// # Safety
/// `b` comes from `lrt_builder_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_builder_push_uint(b: *mut Builder, value: i64) {
    let mut buffer = [0u8; 20];
    let at = format_u64(value as u64, &mut buffer);
    unsafe { builder(b).push(&buffer[at..]) };
}

/// # Safety
/// `b` comes from `lrt_builder_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_builder_push_float(b: *mut Builder, value: f64) {
    let mut text = [0; lang_format::FLOAT_REPR_MAX];
    let len = lang_format::write_float_repr(value, &mut text);
    unsafe { builder(b).push(&text[..len]) };
}

/// # Safety
/// `b` comes from `lrt_builder_new`; `ptr` and `len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_builder_push_str_repr(b: *mut Builder, ptr: *const u8, len: i64) {
    let b = unsafe { builder(b) };
    write_str_repr(unsafe { Str::from_raw(ptr, len) }, |bytes| b.push(bytes));
}

/// The compiler validated the spec, so parsing cannot fail here.
unsafe fn spec(ptr: *const u8, len: i64) -> lang_format::Spec {
    // SAFETY: guaranteed by the caller.
    let text = unsafe { Str::from_raw(ptr, len) };
    lang_format::Spec::parse(text).expect("format spec was validated by the compiler")
}

/// # Safety
/// `b` comes from `lrt_builder_new`; the spec is a UTF-8 string the compiler validated.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_builder_push_format_int(
    b: *mut Builder,
    value: i64,
    spec_ptr: *const u8,
    spec_len: i64,
) {
    let b = unsafe { builder(b) };
    let spec = unsafe { spec(spec_ptr, spec_len) };
    if spec.is_plain() && matches!(spec.kind, None | Some('d')) {
        push_i64(b, value);
    } else {
        b.push(lang_format::format_int(value, &spec).as_bytes());
    }
}

/// # Safety
/// `b` comes from `lrt_builder_new`; the spec is a UTF-8 string the compiler validated.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_builder_push_format_float(
    b: *mut Builder,
    value: f64,
    spec_ptr: *const u8,
    spec_len: i64,
) {
    use std::fmt::Write;
    let b = unsafe { builder(b) };
    let spec = unsafe { spec(spec_ptr, spec_len) };
    match (spec.kind, spec.precision) {
        // The common `{x:.2f}`: written straight into the builder, as `format_float` would.
        (Some('f'), Some(precision)) if spec.is_plain() && value.is_finite() => {
            if value.is_sign_negative() {
                b.push(b"-");
            }
            let _ = write!(b, "{:.precision$}", value.abs());
        }
        _ => b.push(lang_format::format_float(value, &spec).as_bytes()),
    }
}

/// # Safety
/// `b` comes from `lrt_builder_new`; both strings are UTF-8 and the compiler validated the spec.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_builder_push_format_str(
    b: *mut Builder,
    ptr: *const u8,
    len: i64,
    spec_ptr: *const u8,
    spec_len: i64,
) {
    unsafe {
        let text = lang_format::format_str(Str::from_raw(ptr, len), &spec(spec_ptr, spec_len));
        builder(b).push(text.as_bytes());
    }
}

/// Turn the builder's contents into a string. The builder must not be used afterwards.
///
/// # Safety
/// `b` comes from `lrt_builder_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_builder_finish(b: *mut Builder) -> Str {
    let b = unsafe { builder(b) };
    if b.len == 0 { Str::empty() } else { Str { ptr: b.data, len: b.len } }
}

/// Write the builder's contents to standard output.
///
/// # Safety
/// `b` comes from `lrt_builder_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_builder_print(b: *mut Builder) {
    crate::out::write(unsafe { builder(b) }.bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_format_like_to_string() {
        for value in [0, 1, -1, 9, 10, -10, 1234567890, i64::MAX, i64::MIN] {
            let mut b = Builder { data: std::ptr::null_mut(), len: 0, cap: 0 };
            push_i64(&mut b, value);
            assert_eq!(b.bytes(), value.to_string().as_bytes());
        }
        let mut buffer = [0u8; 20];
        let at = format_u64(u64::MAX, &mut buffer);
        assert_eq!(&buffer[at..], u64::MAX.to_string().as_bytes());
    }

    #[test]
    fn repr_matches_python() {
        assert_eq!(str_repr("abc"), "'abc'");
        assert_eq!(str_repr("it's"), "\"it's\"");
        assert_eq!(str_repr("both ' and \""), "'both \\' and \"'");
        assert_eq!(str_repr("a\nb\\"), "'a\\nb\\\\'");
        assert_eq!(str_repr("héllo"), "'héllo'");
        assert_eq!(str_repr("\u{1}"), "'\\x01'");
    }
}
