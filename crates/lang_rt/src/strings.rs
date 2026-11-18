//! String operations. Strings are immutable UTF-8; operations that keep a part of a string (slices,
//! `strip`, `split`) share the original bytes instead of copying, which the collector supports
//! because it recognises pointers into the middle of an object.
//!
//! Offsets (`find`, slicing) are byte offsets, as documented in docs/01-language-design.md.

use crate::list::{self, List};
use crate::panic::panic_at;
use crate::{Str, TypeDesc, gc, location};
use std::hash::{BuildHasher, Hasher, RandomState};
use std::sync::OnceLock;

unsafe fn s<'a>(ptr: *const u8, len: i64) -> &'a str {
    unsafe { Str::from_raw(ptr, len) }
}

/// A sub-slice of a string that shares its bytes.
fn share(part: &str) -> Str {
    if part.is_empty() { Str::empty() } else { Str { ptr: part.as_ptr(), len: part.len() as i64 } }
}

/// # Safety
/// Both strings must be valid (pointer, length) pairs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lrt_str_concat(ap: *const u8, al: i64, bp: *const u8, bl: i64) -> Str {
    if al == 0 {
        return Str { ptr: bp, len: bl };
    }
    if bl == 0 {
        return Str { ptr: ap, len: al };
    }
    let total = (al + bl) as usize;
    let ptr = gc::alloc_atomic(total);
    // SAFETY: `ptr` has room for both strings; the sources are valid by contract.
    unsafe {
        std::ptr::copy_nonoverlapping(ap, ptr, al as usize);
        std::ptr::copy_nonoverlapping(bp, ptr.add(al as usize), bl as usize);
    }
    Str { ptr, len: total as i64 }
}

/// # Safety
/// Both strings must be valid (pointer, length) pairs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lrt_str_eq(ap: *const u8, al: i64, bp: *const u8, bl: i64) -> i64 {
    (al == bl && unsafe { s(ap, al) == s(bp, bl) }) as i64
}

/// Byte-wise comparison, which for UTF-8 is the same as comparing code points.
///
/// # Safety
/// Both strings must be valid (pointer, length) pairs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lrt_str_cmp(ap: *const u8, al: i64, bp: *const u8, bl: i64) -> i64 {
    match unsafe { s(ap, al).cmp(s(bp, bl)) } {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

/// A random seed per process, so that hash values cannot be predicted from outside and request
/// data cannot be crafted to collide.
fn seed() -> u64 {
    static SEED: OnceLock<u64> = OnceLock::new();
    *SEED.get_or_init(|| {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u64(0x5eed);
        hasher.finish()
    })
}

/// 64x64 to 128-bit multiply, folded back to 64 bits.
fn fold_mul(a: u64, b: u64) -> u64 {
    let product = (a as u128) * (b as u128);
    (product as u64) ^ ((product >> 64) as u64)
}

/// A fast seeded hash in the style of wyhash, the family Go's runtime uses for map keys. It is
/// not cryptographic; resistance to crafted collisions comes from the random seed, as in Go.
/// About an order of magnitude faster than SipHash on short keys, which dominate in practice.
pub fn hash_bytes(bytes: &[u8], seed: u64) -> u64 {
    const P0: u64 = 0xa076_1d64_78bd_642f;
    const P1: u64 = 0xe703_7ed1_a0b4_28db;
    let mut h = seed ^ fold_mul(bytes.len() as u64 ^ P0, P1);
    let mut chunks = bytes.chunks_exact(8);
    for chunk in &mut chunks {
        let word = u64::from_le_bytes(chunk.try_into().unwrap());
        h = fold_mul(h ^ word, P1 ^ seed);
    }
    let rest = chunks.remainder();
    if !rest.is_empty() {
        let mut buffer = [0u8; 8];
        buffer[..rest.len()].copy_from_slice(rest);
        h = fold_mul(h ^ u64::from_le_bytes(buffer), P0 ^ seed);
    }
    fold_mul(h, P1)
}

/// # Safety
/// The string must be a valid (pointer, length) pair.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lrt_str_hash(ptr: *const u8, len: i64) -> i64 {
    hash_bytes(unsafe { s(ptr, len) }.as_bytes(), seed()) as i64
}

/// Hash of the `str` stored at `key`. Used directly as a dict's hash function for `str` keys,
/// saving a call through generated code on every probe.
///
/// # Safety
/// `key` must point to a `Str`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lrt_str_hash_ptr(key: *const u8) -> i64 {
    let key = unsafe { *(key as *const Str) };
    unsafe { lrt_str_hash(key.ptr, key.len) }
}

/// Equality of the `str` values at `a` and `b`; the dict equality function for `str` keys.
///
/// # Safety
/// Both must point to `Str` values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lrt_str_eq_ptr(a: *const u8, b: *const u8) -> i64 {
    let (a, b) = unsafe { (*(a as *const Str), *(b as *const Str)) };
    unsafe { lrt_str_eq(a.ptr, a.len, b.ptr, b.len) }
}

/// Byte offset of the first occurrence of `needle`, or -1.
///
/// # Safety
/// Both strings must be valid (pointer, length) pairs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lrt_str_find(hp: *const u8, hl: i64, np: *const u8, nl: i64) -> i64 {
    unsafe { s(hp, hl).find(s(np, nl)) }.map_or(-1, |i| i as i64)
}

fn clamp_slice(len: i64, start: i64, stop: i64, has_start: i64, has_stop: i64) -> (i64, i64) {
    let bound = |value: i64, present: i64, default: i64| {
        if present == 0 {
            default
        } else if value < 0 {
            (value + len).max(0)
        } else {
            value.min(len)
        }
    };
    let start = bound(start, has_start, 0);
    let stop = bound(stop, has_stop, len).max(start);
    (start, stop)
}

/// `s[start:stop]` by byte offsets, with Python's clamping of out-of-range bounds.
///
/// # Safety
/// The string and the location must be valid (pointer, length) pairs.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn lrt_str_slice(
    ptr: *const u8,
    len: i64,
    start: i64,
    stop: i64,
    has_start: i64,
    has_stop: i64,
    loc: *const u8,
    loc_len: i64,
) -> Str {
    let text = unsafe { s(ptr, len) };
    let (start, stop) = clamp_slice(len, start, stop, has_start, has_stop);
    for bound in [start, stop] {
        if !text.is_char_boundary(bound as usize) {
            let message = format!("string slice index {bound} is inside a character");
            panic_at(&message, unsafe { location(loc, loc_len) });
        }
    }
    share(&text[start as usize..stop as usize])
}

macro_rules! str_to_str {
    ($name:ident, $body:expr) => {
        /// # Safety
        /// The string must be a valid (pointer, length) pair.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(ptr: *const u8, len: i64) -> Str {
            let f: fn(&str) -> Str = $body;
            f(unsafe { s(ptr, len) })
        }
    };
}

/// Change case. Strings are immutable, so text that is already in the right case is returned
/// as it is, and ASCII text is converted with a single allocation.
fn change_case(text: &str, upper: bool) -> Str {
    let unchanged =
        if upper { !text.chars().any(|c| c.is_lowercase()) } else { !text.chars().any(|c| c.is_uppercase()) };
    if unchanged {
        return share(text);
    }
    if text.is_ascii() {
        let ptr = gc::alloc_atomic(text.len());
        for (i, byte) in text.bytes().enumerate() {
            let converted = if upper { byte.to_ascii_uppercase() } else { byte.to_ascii_lowercase() };
            // SAFETY: `ptr` has `text.len()` bytes.
            unsafe { *ptr.add(i) = converted };
        }
        return Str { ptr, len: text.len() as i64 };
    }
    let converted = if upper { text.to_uppercase() } else { text.to_lowercase() };
    Str::from_bytes(converted.as_bytes())
}

str_to_str!(lrt_str_upper, |t| change_case(t, true));
str_to_str!(lrt_str_lower, |t| change_case(t, false));
str_to_str!(lrt_str_strip, |t| share(t.trim()));
str_to_str!(lrt_str_lstrip, |t| share(t.trim_start()));
str_to_str!(lrt_str_rstrip, |t| share(t.trim_end()));

fn list_of_strs<'a>(desc: *const TypeDesc, parts: impl Iterator<Item = &'a str>) -> *mut List {
    let list = list::new(desc, 0);
    for part in parts {
        // SAFETY: `list` is a list of `str`, so each slot holds a `Str`.
        unsafe { *(list::push_slot(list) as *mut Str) = share(part) };
    }
    list
}

/// `s.split(sep)`.
///
/// # Safety
/// The strings and location must be valid (pointer, length) pairs; `desc` describes `list[str]`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lrt_str_split(
    ptr: *const u8,
    len: i64,
    sep: *const u8,
    sep_len: i64,
    desc: *const TypeDesc,
    loc: *const u8,
    loc_len: i64,
) -> *mut List {
    if sep_len == 0 {
        panic_at("empty separator passed to split()", unsafe { location(loc, loc_len) });
    }
    list_of_strs(desc, unsafe { s(ptr, len).split(s(sep, sep_len)) })
}

/// `s.split()`: split on runs of whitespace, dropping empty parts.
///
/// # Safety
/// The string must be a valid (pointer, length) pair; `desc` describes `list[str]`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lrt_str_split_ws(ptr: *const u8, len: i64, desc: *const TypeDesc) -> *mut List {
    let text = unsafe { s(ptr, len) };
    // ASCII text, the common case, can be split byte by byte.
    if text.is_ascii() {
        list_of_strs(desc, text.split_ascii_whitespace())
    } else {
        list_of_strs(desc, text.split_whitespace())
    }
}

/// `sep.join(items)` for a `list[str]`.
///
/// # Safety
/// `sep` must be a valid (pointer, length) pair and `items` a `list[str]`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lrt_str_join(sep: *const u8, sep_len: i64, items: *mut List) -> Str {
    let sep = unsafe { s(sep, sep_len) };
    let count = unsafe { (*items).len } as usize;
    let part = |i: usize| unsafe {
        let item = *(list::slot(items, i) as *const Str);
        s(item.ptr, item.len)
    };
    if count == 0 {
        return Str::empty();
    }
    if count == 1 {
        return share(part(0));
    }
    let total = (0..count).map(|i| part(i).len()).sum::<usize>() + sep.len() * (count - 1);
    let ptr = gc::alloc_atomic(total);
    let mut at = 0;
    for i in 0..count {
        let piece = if i == 0 { part(i).to_string() } else { format!("{sep}{}", part(i)) };
        // SAFETY: the total size was computed from the same pieces.
        unsafe { std::ptr::copy_nonoverlapping(piece.as_ptr(), ptr.add(at), piece.len()) };
        at += piece.len();
    }
    Str { ptr, len: total as i64 }
}

/// # Safety
/// All three strings must be valid (pointer, length) pairs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lrt_str_replace(
    ptr: *const u8,
    len: i64,
    old: *const u8,
    old_len: i64,
    new: *const u8,
    new_len: i64,
) -> Str {
    let (text, old, new) = unsafe { (s(ptr, len), s(old, old_len), s(new, new_len)) };
    if !text.contains(old) && !old.is_empty() {
        return Str { ptr, len };
    }
    Str::from_bytes(text.replace(old, new).as_bytes())
}

/// # Safety
/// Both strings must be valid (pointer, length) pairs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lrt_str_starts_with(ptr: *const u8, len: i64, p: *const u8, pl: i64) -> i64 {
    unsafe { s(ptr, len).starts_with(s(p, pl)) as i64 }
}

/// # Safety
/// Both strings must be valid (pointer, length) pairs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lrt_str_ends_with(ptr: *const u8, len: i64, p: *const u8, pl: i64) -> i64 {
    unsafe { s(ptr, len).ends_with(s(p, pl)) as i64 }
}

/// The character at byte offset `*offset`, as a one-character string, advancing the offset. At
/// the end, returns a string with a null pointer. Used by `for c in text`.
///
/// # Safety
/// The string must be a valid (pointer, length) pair and `*offset` a character boundary.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lrt_str_next_char(ptr: *const u8, len: i64, offset: *mut i64) -> Str {
    let text = unsafe { s(ptr, len) };
    let at = unsafe { *offset } as usize;
    match text[at..].chars().next() {
        Some(c) => {
            unsafe { *offset += c.len_utf8() as i64 };
            share(&text[at..at + c.len_utf8()])
        }
        None => Str { ptr: std::ptr::null(), len: 0 },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_depends_on_every_byte_and_the_length() {
        let base = hash_bytes(b"hello world", 1);
        assert_eq!(base, hash_bytes(b"hello world", 1));
        assert_ne!(base, hash_bytes(b"hello worle", 1));
        assert_ne!(base, hash_bytes(b"hello world", 2));
        assert_ne!(hash_bytes(b"a", 1), hash_bytes(b"a\0", 1));
        assert_ne!(hash_bytes(b"", 1), hash_bytes(b"\0", 1));
    }

    #[test]
    fn slices_clamp_like_python() {
        assert_eq!(clamp_slice(5, 1, 3, 1, 1), (1, 3));
        assert_eq!(clamp_slice(5, -2, 0, 1, 0), (3, 5));
        assert_eq!(clamp_slice(5, 0, 99, 0, 1), (0, 5));
        assert_eq!(clamp_slice(5, 4, 2, 1, 1), (4, 4));
        assert_eq!(clamp_slice(5, -99, -99, 1, 1), (0, 0));
    }
}
