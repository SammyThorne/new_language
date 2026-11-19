//! Number operations too involved to emit inline.

use crate::panic::panic_at;
use crate::{Str, location};

/// `base ** exp` for integers, wrapping on overflow like the other integer operators.
///
/// # Safety
/// `loc` and `loc_len` must describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_int_pow(base: i64, exp: i64, loc: *const u8, loc_len: i64) -> i64 {
    if exp < 0 {
        // SAFETY: guaranteed by the caller.
        panic_at("negative exponent for `int`; convert to `float` first", unsafe { location(loc, loc_len) });
    }
    let (mut result, mut base, mut exp) = (1i64, base, exp as u64);
    while exp > 0 {
        if exp & 1 == 1 {
            result = result.wrapping_mul(base);
        }
        base = base.wrapping_mul(base);
        exp >>= 1;
    }
    result
}

/// `int(x)` for a float: truncates toward zero.
///
/// # Safety
/// `loc` and `loc_len` must describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_float_to_int(x: f64, loc: *const u8, loc_len: i64) -> i64 {
    // 2^63 is exactly representable; every float below it in magnitude fits in an i64.
    const LIMIT: f64 = 9_223_372_036_854_775_808.0;
    if x.is_nan() || x >= LIMIT || x < -LIMIT {
        let message = format!("cannot convert {} to `int`", lang_format::float_repr(x));
        // SAFETY: guaranteed by the caller.
        panic_at(&message, unsafe { location(loc, loc_len) });
    }
    x as i64
}

/// Remove underscores that sit between two digits, as Python allows in numbers (`1_000`).
/// Returns `None` if an underscore is anywhere else.
fn strip_digit_separators(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'_' {
            let between =
                i > 0 && i + 1 < bytes.len() && bytes[i - 1].is_ascii_digit() && bytes[i + 1].is_ascii_digit();
            if !between {
                return None;
            }
        } else {
            out.push(b as char);
        }
    }
    Some(out)
}

/// The value of `int(text)` in Python: surrounding whitespace and a sign allowed, digits with
/// single underscores between them. Values outside the 64-bit range are rejected.
pub fn parse_int(text: &str) -> Option<i64> {
    let trimmed = text.trim();
    if trimmed.is_empty() || !trimmed.is_ascii() {
        return None;
    }
    let cleaned = strip_digit_separators(trimmed)?;
    let digits = cleaned.strip_prefix(['+', '-']).unwrap_or(&cleaned);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    cleaned.parse().ok()
}

/// The value of `float(text)` in Python, including `inf`, `infinity` and `nan` in any case.
pub fn parse_float(text: &str) -> Option<f64> {
    let trimmed = text.trim();
    if trimmed.is_empty() || !trimmed.is_ascii() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    let unsigned = lower.strip_prefix(['+', '-']).unwrap_or(&lower);
    if matches!(unsigned, "inf" | "infinity" | "nan") {
        let value = if unsigned == "nan" { f64::NAN } else { f64::INFINITY };
        return Some(if lower.starts_with('-') { -value } else { value });
    }
    // Rust accepts a few spellings Python does not ("infinity" was handled above).
    if !unsigned.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'+' | b'-' | b'_')) {
        return None;
    }
    strip_digit_separators(trimmed)?.parse().ok()
}

/// `int.parse(text)`: 1 and the value in `*out`, or 0 if the text is not an integer.
///
/// # Safety
/// The string must be a valid (pointer, length) pair and `out` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_parse_int(ptr: *const u8, len: i64, out: *mut i64) -> i64 {
    match parse_int(unsafe { Str::from_raw(ptr, len) }) {
        Some(value) => {
            unsafe { *out = value };
            1
        }
        None => 0,
    }
}

/// `float.parse(text)`: 1 and the value in `*out`, or 0 if the text is not a number.
///
/// # Safety
/// The string must be a valid (pointer, length) pair and `out` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_parse_float(ptr: *const u8, len: i64, out: *mut f64) -> i64 {
    match parse_float(unsafe { Str::from_raw(ptr, len) }) {
        Some(value) => {
            unsafe { *out = value };
            1
        }
        None => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Expected results are what Python's `int()` and `float()` accept.
    #[test]
    fn parsing_matches_python() {
        assert_eq!(parse_int("42"), Some(42));
        assert_eq!(parse_int("  -7\n"), Some(-7));
        assert_eq!(parse_int("+1_000"), Some(1000));
        assert_eq!(parse_int("1__0"), None);
        assert_eq!(parse_int("_1"), None);
        assert_eq!(parse_int("1.5"), None);
        assert_eq!(parse_int(""), None);
        assert_eq!(parse_int("-"), None);
        assert_eq!(parse_int("9223372036854775808"), None);
        assert_eq!(parse_float("2.5"), Some(2.5));
        assert_eq!(parse_float(" 1e3 "), Some(1000.0));
        assert_eq!(parse_float("-Infinity"), Some(f64::NEG_INFINITY));
        assert!(parse_float("nan").unwrap().is_nan());
        assert_eq!(parse_float("1_0.5"), Some(10.5));
        assert_eq!(parse_float("abc"), None);
        assert_eq!(parse_float("1,5"), None);
    }

    #[test]
    fn pow_wraps() {
        let loc = "x";
        // SAFETY: `loc` is UTF-8.
        unsafe {
            assert_eq!(lrt_int_pow(2, 10, loc.as_ptr(), 1), 1024);
            assert_eq!(lrt_int_pow(7, 0, loc.as_ptr(), 1), 1);
            assert_eq!(lrt_int_pow(2, 64, loc.as_ptr(), 1), 0);
            assert_eq!(lrt_int_pow(-3, 3, loc.as_ptr(), 1), -27);
        }
    }
}
