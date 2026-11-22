//! The `crypto` module (docs/13, milestone 2): random bytes, SHA-256, HMAC-SHA256, a
//! constant-time comparison, base64, and RS256 signature checks, on `ring`, which the runtime
//! already uses for TLS. Data arrives and leaves as a `bytes` value's (pointer, length), which
//! need not be UTF-8; base64 text is ASCII.

use crate::Str;
use ring::{digest, hmac, rand::SecureRandom, signature};

/// # Safety
/// `ptr` and `len` describe a valid byte range (or `len` is 0).
unsafe fn data<'a>(ptr: *const u8, len: i64) -> &'a [u8] {
    if len <= 0 { &[] } else { unsafe { std::slice::from_raw_parts(ptr, len as usize) } }
}

/// `None` for a `bytes?` when there is no value.
fn none() -> Str {
    Str { ptr: std::ptr::null(), len: 0 }
}

/// `n` bytes from the operating system's secure random source.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_crypto_random(n: i64) -> Str {
    let mut buffer = vec![0u8; n.max(0) as usize];
    if ring::rand::SystemRandom::new().fill(&mut buffer).is_err() {
        crate::panic::panic_at("the operating system gave no random bytes", "");
    }
    Str::from_bytes(&buffer)
}

/// The SHA-256 digest: 32 bytes.
///
/// # Safety
/// `ptr` and `len` describe a valid byte range.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_crypto_sha256(ptr: *const u8, len: i64) -> Str {
    Str::from_bytes(digest::digest(&digest::SHA256, unsafe { data(ptr, len) }).as_ref())
}

/// HMAC-SHA256 of the data with the key: 32 bytes.
///
/// # Safety
/// Both pointer and length pairs describe valid byte ranges.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_crypto_hmac_sha256(
    key_ptr: *const u8,
    key_len: i64,
    ptr: *const u8,
    len: i64,
) -> Str {
    let key = hmac::Key::new(hmac::HMAC_SHA256, unsafe { data(key_ptr, key_len) });
    Str::from_bytes(hmac::sign(&key, unsafe { data(ptr, len) }).as_ref())
}

/// Whether two byte strings are equal, in a time that depends only on their lengths, so a
/// secret compared with a guess does not leak how much of the guess was right. 1 or 0.
///
/// # Safety
/// Both pointer and length pairs describe valid byte ranges.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_crypto_equal(a_ptr: *const u8, a_len: i64, b_ptr: *const u8, b_len: i64) -> i64 {
    i64::from(equal(unsafe { data(a_ptr, a_len) }, unsafe { data(b_ptr, b_len) }))
}

pub(crate) fn equal(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let difference = a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y));
    std::hint::black_box(difference) == 0
}

const STANDARD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const URL_SAFE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn encode(input: &[u8], alphabet: &[u8; 64], pad: bool) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        let symbols = chunk.len() + 1;
        for i in 0..4 {
            if i < symbols {
                out.push(alphabet[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else if pad {
                out.push('=');
            }
        }
    }
    out
}

/// Standard base64 with padding, for tests that build what `base64_decode` reads.
#[cfg(test)]
pub(crate) fn base64_encode(input: &[u8]) -> String {
    encode(input, STANDARD, true)
}

/// Decode either alphabet's text; `=` padding is optional, but must be right if present.
/// `None` for any other character, or a length no encoding produces.
/// Standard base64, padded or not.
pub(crate) fn base64_decode(text: &[u8]) -> Option<Vec<u8>> {
    decode(text, STANDARD)
}

fn decode(text: &[u8], alphabet: &[u8; 64]) -> Option<Vec<u8>> {
    let unpadded = match text.iter().position(|&b| b == b'=') {
        Some(at) => {
            let padding = &text[at..];
            if text.len() % 4 != 0 || padding.len() > 2 || padding.iter().any(|&b| b != b'=') {
                return None;
            }
            &text[..at]
        }
        None => text,
    };
    if unpadded.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(unpadded.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0;
    for &symbol in unpadded {
        let value = alphabet.iter().position(|&a| a == symbol)? as u32;
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    // Left-over bits must be zero, so each byte string has one encoding.
    if buffer != 0 {
        return None;
    }
    Some(out)
}

/// Base64 text of the data: the standard alphabet with `=` padding, or, when `url` is not 0, the
/// URL-safe alphabet (`-` and `_`) without padding, as JSON Web Tokens and cookies use.
///
/// # Safety
/// `ptr` and `len` describe a valid byte range.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_crypto_base64_encode(ptr: *const u8, len: i64, url: i64) -> Str {
    let input = unsafe { data(ptr, len) };
    let text = if url != 0 { encode(input, URL_SAFE, false) } else { encode(input, STANDARD, true) };
    Str::from_bytes(text.as_bytes())
}

/// The data base64 text holds, or `None` if it is not base64 of that alphabet.
///
/// # Safety
/// `ptr` and `len` describe a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_crypto_base64_decode(ptr: *const u8, len: i64, url: i64) -> Str {
    let text = unsafe { data(ptr, len) };
    match decode(text, if url != 0 { URL_SAFE } else { STANDARD }) {
        Some(bytes) if bytes.is_empty() => Str::empty(),
        Some(bytes) => Str::from_bytes(&bytes),
        None => none(),
    }
}

/// Whether `signature` is a valid RSASSA-PKCS1-v1_5 SHA-256 signature (JWT's `RS256`) of
/// `message`, by the public key with this modulus and exponent (big-endian, as a JSON Web Key's
/// `n` and `e` decode to). Keys of 2048 to 8192 bits. 1 or 0.
///
/// # Safety
/// Each pointer and length pair describes a valid byte range.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_crypto_verify_rs256(
    n_ptr: *const u8,
    n_len: i64,
    e_ptr: *const u8,
    e_len: i64,
    message_ptr: *const u8,
    message_len: i64,
    signature_ptr: *const u8,
    signature_len: i64,
) -> i64 {
    let (n, e) = unsafe { (data(n_ptr, n_len), data(e_ptr, e_len)) };
    let (message, sig) = unsafe { (data(message_ptr, message_len), data(signature_ptr, signature_len)) };
    i64::from(verify_rs256(n, e, message, sig))
}

/// Check an ES256 signature (ECDSA with P-256 and SHA-256, as JSON Web Signatures write it: the
/// 32-byte `r` and `s` joined) with the public key's point coordinates `x` and `y`. Returns 1 if
/// it is valid.
///
/// # Safety
/// Each pointer and length pair describes a valid byte range.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_crypto_verify_es256(
    x_ptr: *const u8,
    x_len: i64,
    y_ptr: *const u8,
    y_len: i64,
    message_ptr: *const u8,
    message_len: i64,
    signature_ptr: *const u8,
    signature_len: i64,
) -> i64 {
    let (x, y) = unsafe { (data(x_ptr, x_len), data(y_ptr, y_len)) };
    let (message, sig) = unsafe { (data(message_ptr, message_len), data(signature_ptr, signature_len)) };
    i64::from(verify_es256(x, y, message, sig))
}

fn verify_es256(x: &[u8], y: &[u8], message: &[u8], sig: &[u8]) -> bool {
    if x.len() != 32 || y.len() != 32 || sig.len() != 64 {
        return false;
    }
    // The uncompressed point: 0x04, then x and y.
    let mut point = Vec::with_capacity(65);
    point.push(4);
    point.extend_from_slice(x);
    point.extend_from_slice(y);
    signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, point).verify(message, sig).is_ok()
}

/// Without leading zero bytes, which a key's numbers may have but `ring` refuses.
fn strip(bytes: &[u8]) -> &[u8] {
    bytes.iter().position(|&b| b != 0).map_or(&[], |at| &bytes[at..])
}

fn verify_rs256(n: &[u8], e: &[u8], message: &[u8], sig: &[u8]) -> bool {
    let key = signature::RsaPublicKeyComponents { n: strip(n), e: strip(e) };
    key.verify(&signature::RSA_PKCS1_2048_8192_SHA256, message, sig).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn es256_signatures_are_checked() {
        use ring::rand::SystemRandom;
        use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair};
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng).unwrap();
        let public = pair.public_key().as_ref().to_vec();
        let (x, y) = (&public[1..33], &public[33..65]);
        let sig = pair.sign(&rng, b"header.claims").unwrap();
        assert!(verify_es256(x, y, b"header.claims", sig.as_ref()));
        assert!(!verify_es256(x, y, b"header.other", sig.as_ref()));
        let mut bent = sig.as_ref().to_vec();
        bent[10] ^= 1;
        assert!(!verify_es256(x, y, b"header.claims", &bent));
        assert!(!verify_es256(x, y, b"header.claims", &bent[..63]));
    }

    #[test]
    fn base64_known_answers() {
        // RFC 4648, section 10.
        let cases = [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ];
        for (plain, encoded) in cases {
            assert_eq!(encode(plain.as_bytes(), STANDARD, true), encoded);
            assert_eq!(decode(encoded.as_bytes(), STANDARD).as_deref(), Some(plain.as_bytes()));
            let url = encoded.trim_end_matches('=');
            assert_eq!(encode(plain.as_bytes(), URL_SAFE, false), url);
            assert_eq!(decode(url.as_bytes(), URL_SAFE).as_deref(), Some(plain.as_bytes()));
        }
        assert_eq!(encode(&[0xfb, 0xff], STANDARD, true), "+/8=");
        assert_eq!(encode(&[0xfb, 0xff], URL_SAFE, false), "-_8");
    }

    #[test]
    fn base64_rejects_what_no_encoder_writes() {
        for bad in ["Zg=", "Z", "Zm9v!", "Zh==", "Zg===", "Z=g=", "-_8"] {
            assert_eq!(decode(bad.as_bytes(), STANDARD), None, "{bad}");
        }
        assert_eq!(decode(b"+/8=", URL_SAFE), None);
    }

    #[test]
    fn equal_compares_whole_strings() {
        assert!(equal(b"abc", b"abc"));
        assert!(equal(b"", b""));
        assert!(!equal(b"abc", b"abd"));
        assert!(!equal(b"abc", b"abcd"));
    }

    #[test]
    fn digests_known_answers() {
        let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(
            hex(digest::digest(&digest::SHA256, b"abc").as_ref()),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // RFC 4231, test case 2.
        let key = hmac::Key::new(hmac::HMAC_SHA256, b"Jefe");
        assert_eq!(
            hex(hmac::sign(&key, b"what do ya want for nothing?").as_ref()),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }
}
