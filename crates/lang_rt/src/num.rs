//! Number operations too involved to emit inline.

use crate::location;
use crate::panic::panic_at;

/// `base ** exp` for integers, wrapping on overflow like the other integer operators.
///
/// # Safety
/// `loc` and `loc_len` must describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lrt_int_pow(base: i64, exp: i64, loc: *const u8, loc_len: i64) -> i64 {
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
pub unsafe extern "C" fn lrt_float_to_int(x: f64, loc: *const u8, loc_len: i64) -> i64 {
    // 2^63 is exactly representable; every float below it in magnitude fits in an i64.
    const LIMIT: f64 = 9_223_372_036_854_775_808.0;
    if x.is_nan() || x >= LIMIT || x < -LIMIT {
        let message = format!("cannot convert {} to `int`", lang_format::float_repr(x));
        // SAFETY: guaranteed by the caller.
        panic_at(&message, unsafe { location(loc, loc_len) });
    }
    x as i64
}

#[cfg(test)]
mod tests {
    use super::*;

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
