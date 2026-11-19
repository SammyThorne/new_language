//! Operations behind the prelude's `bytes` type. A `bytes` value keeps its data in a string's
//! (pointer, length) representation, but the data need not be UTF-8, so these functions work on
//! bytes and never treat the data as text.

use crate::Str;

/// # Safety
/// `ptr` and `len` describe a valid byte range (or `len` is 0).
unsafe fn data<'a>(ptr: *const u8, len: i64) -> &'a [u8] {
    if len <= 0 { &[] } else { unsafe { std::slice::from_raw_parts(ptr, len as usize) } }
}

/// The byte at `index` (0 to 255), or -1 if the index is out of range.
///
/// # Safety
/// `ptr` and `len` describe a valid byte range.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_bytes_at(ptr: *const u8, len: i64, index: i64) -> i64 {
    let bytes = unsafe { data(ptr, len) };
    let index = if index < 0 { index + len } else { index };
    if (0..len).contains(&index) { bytes[index as usize] as i64 } else { -1 }
}

/// Bytes `start` to `end`, clamped to the data as Python slices are; shares the data.
///
/// # Safety
/// `ptr` and `len` describe a valid byte range.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_bytes_slice(ptr: *const u8, len: i64, start: i64, end: i64) -> Str {
    let clamp = |i: i64| if i < 0 { (i + len).max(0) } else { i.min(len) };
    let (start, end) = (clamp(start), clamp(end));
    if start >= end {
        return Str::empty();
    }
    Str { ptr: unsafe { ptr.add(start as usize) }, len: end - start }
}

/// 1 if the data is valid UTF-8.
///
/// # Safety
/// `ptr` and `len` describe a valid byte range.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_bytes_is_utf8(ptr: *const u8, len: i64) -> i64 {
    std::str::from_utf8(unsafe { data(ptr, len) }).is_ok() as i64
}

/// The data written as Python writes a bytes literal: `b'ab\x00'`.
///
/// # Safety
/// `ptr` and `len` describe a valid byte range.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_bytes_repr(ptr: *const u8, len: i64) -> Str {
    let bytes = unsafe { data(ptr, len) };
    let quote = if bytes.contains(&b'\'') && !bytes.contains(&b'"') { '"' } else { '\'' };
    let mut out = format!("b{quote}");
    for &b in bytes {
        match b {
            b'\\' => out.push_str("\\\\"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            _ if b as char == quote => {
                out.push('\\');
                out.push(quote);
            }
            0x20..=0x7E => out.push(b as char),
            _ => out.push_str(&format!("\\x{b:02x}")),
        }
    }
    out.push(quote);
    Str::from_bytes(out.as_bytes())
}

/// The data as lower-case hexadecimal.
///
/// # Safety
/// `ptr` and `len` describe a valid byte range.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_bytes_hex(ptr: *const u8, len: i64) -> Str {
    let text: String = unsafe { data(ptr, len) }.iter().map(|b| format!("{b:02x}")).collect();
    Str::from_bytes(text.as_bytes())
}

/// One byte (the low 8 bits of `value`) as a one-byte `bytes`.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_bytes_of(value: i64) -> Str {
    Str::from_bytes(&[value as u8])
}
