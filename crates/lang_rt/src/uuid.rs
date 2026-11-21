//! UUIDs for the `uuid` module (docs/14, milestone 6): random version 4, time-ordered version 7,
//! and parsing. A UUID crosses into the language as its 36-character lowercase text, which is
//! also how the module stores it: equal UUIDs have equal text, and the text sorts as the bytes
//! do.

use std::cell::RefCell;
use std::sync::Mutex;

use ring::rand::{SecureRandom, SystemRandom};

use crate::Str;
use crate::sys::record_error;

/// # Safety
/// `ptr` and `len` describe a valid byte range (or `len` is 0).
unsafe fn data<'a>(ptr: *const u8, len: i64) -> &'a [u8] {
    if len <= 0 { &[] } else { unsafe { std::slice::from_raw_parts(ptr, len as usize) } }
}

/// `None` for a `str?`.
fn none() -> Str {
    Str { ptr: std::ptr::null(), len: 0 }
}

/// Bytes from the operating system's secure random source, read 4 KiB at a time per thread, so
/// that a UUID (one per request, with `http.request_log`) costs a system call only once in
/// about 400.
fn random<const N: usize>() -> [u8; N] {
    const SIZE: usize = 4096;
    thread_local! {
        static POOL: RefCell<(Box<[u8; SIZE]>, usize)> = RefCell::new((Box::new([0; SIZE]), SIZE));
    }
    POOL.with_borrow_mut(|(pool, used)| {
        if *used + N > SIZE {
            if SystemRandom::new().fill(&mut pool[..]).is_err() {
                crate::panic::panic_at("the operating system gave no random bytes", "");
            }
            *used = 0;
        }
        let mut bytes = [0u8; N];
        bytes.copy_from_slice(&pool[*used..*used + N]);
        // Bytes handed out are not kept.
        pool[*used..*used + N].fill(0);
        *used += N;
        bytes
    })
}

/// The hyphenated lowercase form: 8-4-4-4-12 hex digits.
fn text(bytes: &[u8; 16]) -> String {
    let mut out = String::with_capacity(36);
    for (i, b) in bytes.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            out.push('-');
        }
        out.push(char::from(b"0123456789abcdef"[usize::from(b >> 4)]));
        out.push(char::from(b"0123456789abcdef"[usize::from(b & 15)]));
    }
    out
}

fn v4() -> [u8; 16] {
    let mut bytes: [u8; 16] = random();
    bytes[6] = 0x40 | (bytes[6] & 0x0f);
    bytes[8] = 0x80 | (bytes[8] & 0x3f);
    bytes
}

/// The counter's width: the 12 bits of `rand_a` and the top 30 bits of `rand_b`.
const COUNTER_BITS: u32 = 42;

/// The millisecond of the last version 7 UUID, and the counter it used.
static LAST_V7: Mutex<(i64, u64)> = Mutex::new((i64::MIN, 0));

/// A version 7 UUID for the Unix millisecond `now_ms`, later than every one made before in this
/// process (RFC 9562, section 6.2, method 1). Within a millisecond a 42-bit counter counts up
/// from a random start below 2^41, so at least 2^41 more fit before it runs out; then, as when
/// the clock goes backwards, the timestamp is moved on past the last one used. The last 32 bits
/// are random in every UUID.
fn v7(now_ms: i64) -> [u8; 16] {
    let fresh: [u8; 10] = random();
    let start = u64::from_be_bytes([0, 0, fresh[0], fresh[1], fresh[2], fresh[3], fresh[4], fresh[5]])
        & ((1 << (COUNTER_BITS - 1)) - 1);
    let (ms, counter) = {
        let mut last = LAST_V7.lock().unwrap_or_else(|e| e.into_inner());
        let next = if now_ms > last.0 {
            (now_ms, start)
        } else if last.1 + 1 < 1 << COUNTER_BITS {
            (last.0, last.1 + 1)
        } else {
            (last.0 + 1, start)
        };
        *last = next;
        next
    };
    let ms = (ms.max(0) as u64) & ((1 << 48) - 1);
    let mut bytes = [0u8; 16];
    bytes[..6].copy_from_slice(&ms.to_be_bytes()[2..]);
    bytes[6] = 0x70 | ((counter >> 38) & 0x0f) as u8;
    bytes[7] = (counter >> 30) as u8;
    bytes[8] = 0x80 | ((counter >> 24) & 0x3f) as u8;
    bytes[9] = (counter >> 16) as u8;
    bytes[10] = (counter >> 8) as u8;
    bytes[11] = counter as u8;
    bytes[12..].copy_from_slice(&fresh[6..]);
    bytes
}

/// Parse the hyphenated form, in either case; the error says what was wrong.
fn parse(input: &[u8]) -> Result<[u8; 16], String> {
    if input.len() != 36 {
        return Err(format!(
            "a UUID is 36 characters (8-4-4-4-12 hex digits with hyphens), not {}",
            String::from_utf8_lossy(input).chars().count()
        ));
    }
    let mut bytes = [0u8; 16];
    let mut nibbles = 0;
    for (i, &c) in input.iter().enumerate() {
        if matches!(i, 8 | 13 | 18 | 23) {
            if c != b'-' {
                return Err(format!("expected `-` at position {}, found {}", i + 1, shown(input, i)));
            }
            continue;
        }
        let value = match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            _ => return Err(format!("expected a hex digit at position {}, found {}", i + 1, shown(input, i))),
        };
        bytes[nibbles / 2] |= if nibbles % 2 == 0 { value << 4 } else { value };
        nibbles += 1;
    }
    Ok(bytes)
}

/// The character at byte `i`, quoted, for an error message.
fn shown(input: &[u8], i: usize) -> String {
    match std::str::from_utf8(&input[i..]).ok().and_then(|rest| rest.chars().next()) {
        Some(c) => format!("`{c}`"),
        None => "a byte that is not UTF-8".to_string(),
    }
}

/// A random (version 4) UUID's text.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_uuid_v4() -> Str {
    Str::from_bytes(text(&v4()).as_bytes())
}

/// A time-ordered (version 7) UUID's text, at the `datetime` module's clock (which tests can fix).
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_uuid_v7() -> Str {
    let ms = crate::datetime::lrt_dt_now_ns().div_euclid(1_000_000);
    Str::from_bytes(text(&v7(ms)).as_bytes())
}

/// The lowercase text of a UUID written in either case, or `None` after recording why it is not
/// one.
///
/// # Safety
/// `ptr` and `len` describe a valid byte range.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_uuid_parse(ptr: *const u8, len: i64) -> Str {
    match parse(unsafe { data(ptr, len) }) {
        Ok(bytes) => Str::from_bytes(text(&bytes).as_bytes()),
        Err(message) => {
            record_error(message);
            none()
        }
    }
}

/// The text of the UUID made of 16 bytes, or `None` after recording why there are not 16.
///
/// # Safety
/// `ptr` and `len` describe a valid byte range.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_uuid_from_bytes(ptr: *const u8, len: i64) -> Str {
    match <[u8; 16]>::try_from(unsafe { data(ptr, len) }) {
        Ok(bytes) => Str::from_bytes(text(&bytes).as_bytes()),
        Err(_) => {
            record_error(format!("a UUID is 16 bytes, not {}", len.max(0)));
            none()
        }
    }
}

/// The 16 bytes of a UUID's text (which the module has already checked).
///
/// # Safety
/// `ptr` and `len` describe a valid byte range.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_uuid_bytes(ptr: *const u8, len: i64) -> Str {
    Str::from_bytes(&parse(unsafe { data(ptr, len) }).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(bytes: &[u8; 16]) -> u8 {
        bytes[6] >> 4
    }

    fn variant_is_rfc(bytes: &[u8; 16]) -> bool {
        bytes[8] >> 6 == 0b10
    }

    #[test]
    fn version_and_variant_bits() {
        for _ in 0..1000 {
            let a = v4();
            assert_eq!(version(&a), 4);
            assert!(variant_is_rfc(&a));
            let b = v7(1_795_163_400_000);
            assert_eq!(version(&b), 7);
            assert!(variant_is_rfc(&b));
        }
        let t = text(&v4());
        assert_eq!(t.len(), 36);
        assert_eq!(&t[14..15], "4");
    }

    #[test]
    fn v7_carries_the_millisecond() {
        let bytes = v7(1_895_163_400_123);
        let mut ms = [0u8; 8];
        ms[2..].copy_from_slice(&bytes[..6]);
        assert!(u64::from_be_bytes(ms) >= 1_895_163_400_123);
    }

    #[test]
    fn v7_orders_within_one_millisecond_and_when_the_clock_goes_back() {
        // Far in the future, so other tests' calls do not interleave with an earlier timestamp.
        let mut previous = text(&v7(2_000_000_000_000));
        for i in 0..100_000 {
            let ms = if i % 1000 == 999 { 1_999_999_999_000 } else { 2_000_000_000_000 };
            let next = text(&v7(ms));
            assert!(next > previous, "{next} after {previous}");
            previous = next;
        }
    }

    #[test]
    fn counter_overflow_moves_the_timestamp_on() {
        *LAST_V7.lock().unwrap() = (3_000_000_000_000, (1 << COUNTER_BITS) - 1);
        let bytes = v7(3_000_000_000_000);
        let mut ms = [0u8; 8];
        ms[2..].copy_from_slice(&bytes[..6]);
        assert_eq!(u64::from_be_bytes(ms), 3_000_000_000_001);
    }

    #[test]
    fn parsing() {
        let bytes = parse(b"0193A1B2-C3D4-7E5F-8A6B-7C8D9E0F1A2B").unwrap();
        assert_eq!(text(&bytes), "0193a1b2-c3d4-7e5f-8a6b-7c8d9e0f1a2b");
        assert_eq!(parse(text(&bytes).as_bytes()), Ok(bytes));
        assert_eq!(
            text(&parse(b"00000000-0000-0000-0000-000000000000").unwrap()),
            "00000000-0000-0000-0000-000000000000"
        );
        assert_eq!(
            parse(b"0193a1b2c3d47e5f8a6b7c8d9e0f1a2b").unwrap_err(),
            "a UUID is 36 characters (8-4-4-4-12 hex digits with hyphens), not 32"
        );
        assert_eq!(
            parse(b"0193a1b2-c3d4-7e5f-8a6b_7c8d9e0f1a2b").unwrap_err(),
            "expected `-` at position 24, found `_`"
        );
        assert_eq!(
            parse(b"0193a1b2-c3d4-7e5f-8a6b-7c8d9e0f1a2g").unwrap_err(),
            "expected a hex digit at position 36, found `g`"
        );
        assert_eq!(
            parse("0193a1b2-c3d4-7e5f-8a6b-7c8d9e0f1aé".as_bytes()).unwrap_err(),
            "expected a hex digit at position 35, found `é`"
        );
        assert_eq!(
            parse(b"{0193a1b2-c3d4-7e5f-8a6b-7c8d9e0f1a}").unwrap_err(),
            "expected a hex digit at position 1, found `{`"
        );
    }
}
