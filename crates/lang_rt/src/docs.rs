//! The API documentation page's files (docs/12, section 4.5), served by `server.docs` in the
//! `http` module. They are reached only through `lrt_docs_asset`, which only the `docs` code
//! calls, so `--gc-sections` drops them from programs that do not serve documentation.

use crate::Str;

/// The page template, its style sheet, the logic (`core.js`) and the DOM code, in the order the
/// `http` module numbers them.
static ASSETS: [&str; 4] = [
    include_str!("../assets/viewer/index.html"),
    include_str!("../assets/viewer/viewer.css"),
    include_str!("../assets/viewer/core.js"),
    include_str!("../assets/viewer/viewer.js"),
];

/// One of the viewer's files, as a string that needs no copy; empty for an unknown index.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_docs_asset(index: i64) -> Str {
    match usize::try_from(index).ok().and_then(|i| ASSETS.get(i)) {
        Some(text) => Str { ptr: text.as_ptr(), len: text.len() as i64 },
        None => Str::empty(),
    }
}

/// A 64-bit FNV-1a hash of a string, for the `ETag` of a documentation response.
///
/// # Safety
/// `ptr` and `len` describe a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_docs_hash(ptr: *const u8, len: i64) -> i64 {
    let text = unsafe { Str::from_raw(ptr, len) };
    fnv(text.as_bytes()) as i64
}

fn fnv(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assets_are_there_and_small() {
        let total: usize = ASSETS.iter().map(|a| a.len()).sum();
        assert!(total < 60 * 1024, "the viewer is {total} bytes; the plan's budget is 60 KB");
        assert!(ASSETS[0].contains("{{openapi_url}}") && ASSETS[0].contains("{{base}}"));
        assert_eq!(lrt_docs_asset(9).len, 0);
        assert_eq!(fnv(b""), 0xcbf29ce484222325);
        assert_eq!(fnv(b"a"), 0xaf63dc4c8601ec8c);
    }
}
