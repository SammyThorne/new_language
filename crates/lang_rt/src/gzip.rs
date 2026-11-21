//! gzip (RFC 1952) around `miniz_oxide`'s raw deflate: `http.compress` encodes responses with it
//! and the HTTP client decodes them (docs/08, D108).

use crate::Str;

/// The CRC-32 of the gzip trailer (the IEEE polynomial, reflected), a table built at compile time.
const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut n = 0;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        table[n] = c;
        n += 1;
    }
    table
};

pub(crate) fn crc32(data: &[u8]) -> u32 {
    !data.iter().fold(!0u32, |c, &b| CRC_TABLE[((c ^ u32::from(b)) & 0xFF) as usize] ^ (c >> 8))
}

/// `data` as a gzip member, compressed at `level` (0 to 9).
pub(crate) fn encode(data: &[u8], level: u8) -> Vec<u8> {
    // Magic, deflate, no flags, no time, the extra flags for the level, and "unknown" system.
    let extra = match level {
        9 => 2,
        0..=1 => 4,
        _ => 0,
    };
    let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, extra, 255];
    out.extend_from_slice(&miniz_oxide::deflate::compress_to_vec(data, level.min(9)));
    out.extend_from_slice(&crc32(data).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}

/// The data of a gzip body: one or more members, each checked against its trailer. At most
/// `limit` bytes are produced.
pub(crate) fn decode(mut input: &[u8], limit: usize) -> Result<Vec<u8>, String> {
    let bad = || "the gzip body is malformed".to_string();
    let mut out = Vec::new();
    while !input.is_empty() {
        if input.len() < 18 || input[0..3] != [0x1f, 0x8b, 8] {
            return Err(bad());
        }
        let flags = input[3];
        let mut at = 10;
        if flags & 4 != 0 {
            let n =
                usize::from(u16::from_le_bytes([*input.get(at).ok_or_else(bad)?, *input.get(at + 1).ok_or_else(bad)?]));
            at += 2 + n;
        }
        for bit in [8, 16] {
            if flags & bit != 0 {
                at += input.get(at..).ok_or_else(bad)?.iter().position(|&b| b == 0).ok_or_else(bad)? + 1;
            }
        }
        if flags & 2 != 0 {
            at += 2;
        }
        let body = input.get(at..).ok_or_else(bad)?;
        let mut state = miniz_oxide::inflate::stream::InflateState::new_boxed(miniz_oxide::DataFormat::Raw);
        let start = out.len();
        let room = limit.saturating_sub(start);
        out.resize(start + room.min(body.len().saturating_mul(4).max(64 << 10)), 0);
        let mut consumed = 0;
        let mut written = start;
        loop {
            let result = miniz_oxide::inflate::stream::inflate(
                &mut state,
                &body[consumed..],
                &mut out[written..],
                miniz_oxide::MZFlush::None,
            );
            consumed += result.bytes_consumed;
            written += result.bytes_written;
            match result.status {
                Ok(miniz_oxide::MZStatus::StreamEnd) => break,
                Ok(_) | Err(miniz_oxide::MZError::Buf) => {
                    if written == out.len() {
                        if out.len() >= limit {
                            return Err(format!("the gzip body is larger than {limit} bytes"));
                        }
                        let grown = (out.len() * 2).min(limit);
                        out.resize(grown, 0);
                    } else if consumed == body.len() {
                        return Err(bad());
                    }
                }
                Err(_) => return Err(bad()),
            }
        }
        out.truncate(written);
        let trailer = body.get(consumed..consumed + 8).ok_or_else(bad)?;
        let crc = u32::from_le_bytes(trailer[0..4].try_into().unwrap());
        let size = u32::from_le_bytes(trailer[4..8].try_into().unwrap());
        if crc != crc32(&out[start..]) || size != (written - start) as u32 {
            return Err("the gzip body fails its check".to_string());
        }
        input = &body[consumed + 8..];
    }
    Ok(out)
}

/// gzip `data` at `level`, for `http.compress`.
///
/// # Safety
/// The pointer and length describe a byte string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_gzip(data: *const u8, data_len: i64, level: i64) -> Str {
    let data = unsafe { Str::from_raw(data, data_len) };
    Str::from_bytes(&encode(data.as_bytes(), level.clamp(0, 9) as u8))
}

/// The q-value an `Accept-Encoding` header gives `name` itself, or through `*`; `None` if neither
/// is listed.
fn quality(header: &str, name: &str) -> Option<f64> {
    let mut star = None;
    for item in header.split(',') {
        let mut parts = item.split(';');
        let coding = parts.next().unwrap_or("").trim();
        let q = parts
            .filter_map(|p| p.trim().strip_prefix("q=").or_else(|| p.trim().strip_prefix("Q=")))
            .find_map(|v| v.trim().parse::<f64>().ok())
            .unwrap_or(1.0);
        if coding.eq_ignore_ascii_case(name) {
            return Some(q);
        }
        if coding == "*" {
            star = Some(q);
        }
    }
    star
}

/// What `http.compress` may send for an `Accept-Encoding` header: 0 the body as it is, 1 gzip,
/// 2 gzip, and the client refuses the body as it is (`identity;q=0`), so even small bodies of
/// any type are compressed.
///
/// # Safety
/// The pointer and length describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_http_gzip_choice(header: *const u8, header_len: i64) -> i64 {
    let header = unsafe { Str::from_raw(header, header_len) };
    choice(header)
}

fn choice(header: &str) -> i64 {
    if !crate::static_files::accepts_encoding(Some(header), "gzip") {
        0
    } else if quality(header, "identity").is_some_and(|q| q <= 0.0) {
        2
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::{choice, crc32, decode, encode};

    #[test]
    fn round_trips() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        let text = "hello, compression ".repeat(500);
        for level in [0, 1, 6, 9] {
            let packed = encode(text.as_bytes(), level);
            assert_eq!(decode(&packed, usize::MAX).unwrap(), text.as_bytes(), "level {level}");
            if level > 0 {
                assert!(packed.len() < 200, "level {level}: {} bytes", packed.len());
            }
        }
        assert_eq!(decode(&encode(b"", 6), 10).unwrap(), b"");
        // Two members are one body, as `cat a.gz b.gz` makes.
        let mut two = encode(b"one ", 6);
        two.extend_from_slice(&encode(b"two", 6));
        assert_eq!(decode(&two, 100).unwrap(), b"one two");
    }

    #[test]
    fn reads_what_gzip_writes() {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/static/css/");
        let packed = std::fs::read(format!("{root}site.css.gz")).unwrap();
        let plain = std::fs::read(format!("{root}site.css")).unwrap();
        assert_eq!(decode(&packed, 1000).unwrap(), plain);
    }

    #[test]
    fn refuses_bad_and_oversized_bodies() {
        let packed = encode("x".repeat(100_000).as_bytes(), 6);
        assert!(decode(&packed, 1000).unwrap_err().contains("larger than 1000"));
        let mut corrupt = packed.clone();
        let at = corrupt.len() - 6;
        corrupt[at] ^= 1;
        assert!(decode(&corrupt, usize::MAX).is_err());
        assert!(decode(&packed[..packed.len() - 3], usize::MAX).is_err());
        assert!(decode(b"not gzip at all, really", usize::MAX).is_err());
    }

    #[test]
    fn choices() {
        assert_eq!(choice(""), 0);
        assert_eq!(choice("gzip, deflate, br"), 1);
        assert_eq!(choice("br, gzip;q=0"), 0);
        assert_eq!(choice("GZIP;q=0.5"), 1);
        assert_eq!(choice("*"), 1);
        assert_eq!(choice("gzip, identity;q=0"), 2);
        assert_eq!(choice("gzip, *;q=0"), 2);
        assert_eq!(choice("gzip, identity, *;q=0"), 1);
        assert_eq!(choice("identity;q=0"), 0);
    }
}
