//! A growable string buffer in collected memory. Generated code uses it for f-strings, `str(x)`
//! and `print`: create, push pieces, then finish into a `str` or print it.

use crate::{Str, gc};

#[repr(C)]
pub struct Builder {
    data: *mut u8,
    len: i64,
    cap: i64,
}

impl Builder {
    pub fn push(&mut self, bytes: &[u8]) {
        let needed = self.len as usize + bytes.len();
        if needed > self.cap as usize {
            let cap = needed.max(self.cap as usize * 2).max(32);
            let data = gc::alloc_atomic(cap);
            if self.len > 0 {
                // SAFETY: both regions are at least `len` bytes and do not overlap.
                unsafe { std::ptr::copy_nonoverlapping(self.data, data, self.len as usize) };
            }
            self.data = data;
            self.cap = cap as i64;
        }
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
    let quote = if text.contains('\'') && !text.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
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
    unsafe { builder(b).push(value.to_string().as_bytes()) };
}

/// Append an unsigned 64-bit integer (a `u64`).
///
/// # Safety
/// `b` comes from `lrt_builder_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_builder_push_uint(b: *mut Builder, value: i64) {
    unsafe { builder(b).push((value as u64).to_string().as_bytes()) };
}

/// # Safety
/// `b` comes from `lrt_builder_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_builder_push_float(b: *mut Builder, value: f64) {
    unsafe { builder(b).push(lang_format::float_repr(value).as_bytes()) };
}

/// # Safety
/// `b` comes from `lrt_builder_new`; `ptr` and `len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_builder_push_str_repr(b: *mut Builder, ptr: *const u8, len: i64) {
    unsafe { builder(b).push(str_repr(Str::from_raw(ptr, len)).as_bytes()) };
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
    unsafe { builder(b).push(lang_format::format_int(value, &spec(spec_ptr, spec_len)).as_bytes()) };
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
    unsafe { builder(b).push(lang_format::format_float(value, &spec(spec_ptr, spec_len)).as_bytes()) };
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
    fn repr_matches_python() {
        assert_eq!(str_repr("abc"), "'abc'");
        assert_eq!(str_repr("it's"), "\"it's\"");
        assert_eq!(str_repr("both ' and \""), "'both \\' and \"'");
        assert_eq!(str_repr("a\nb\\"), "'a\\nb\\\\'");
        assert_eq!(str_repr("héllo"), "'héllo'");
        assert_eq!(str_repr("\u{1}"), "'\\x01'");
    }
}
