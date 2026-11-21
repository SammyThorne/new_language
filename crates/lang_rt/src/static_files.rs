//! Serving files from a directory (docs/14, milestone 3): `server.static(prefix, root, ...)`.
//!
//! [`resolve`] turns a request path into a decision: a file (or a part of one) to send with its
//! headers, a 304 or 412 or 416, a redirect, or not found. It checks the path segment by
//! segment before touching the file system, then makes sure the file it found is under the
//! root once symbolic links are followed. `http.rs` sends the file.

use std::fs::File;
use std::path::{Path, PathBuf};

/// What `server.static` was given.
pub(crate) struct Options<'a> {
    pub root: &'a str,
    pub index: &'a str,
    pub fallback: &'a str,
    pub dotfiles: bool,
    pub max_age: i64,
    pub immutable: bool,
}

/// The request's headers that matter here; `None` when absent.
#[derive(Default)]
pub(crate) struct Conditions<'a> {
    pub if_match: Option<&'a str>,
    pub if_none_match: Option<&'a str>,
    pub if_modified_since: Option<&'a str>,
    pub if_unmodified_since: Option<&'a str>,
    pub range: Option<&'a str>,
    pub if_range: Option<&'a str>,
    pub accept_encoding: Option<&'a str>,
}

/// A part of a file to send: `len` bytes from `start`.
pub(crate) struct Part {
    pub file: File,
    pub start: u64,
    pub len: u64,
}

pub(crate) enum Outcome {
    NotFound,
    /// A response, with `Name: value\r\n` header lines, and the bytes to send for 200 and 206.
    Respond {
        status: u16,
        content_type: &'static str,
        headers: String,
        body: Option<Part>,
    },
}

/// The longest path accepted, and the longest segment (most file systems' limit).
const MAX_PATH: usize = 4096;
const MAX_SEGMENT: usize = 255;

/// The checked segments of a path below the root, and whether it ended with `/`.
struct Relative {
    segments: Vec<String>,
    directory: bool,
}

/// Check `raw` (the path below the prefix, as sent: percent-encoded, without a leading `/`).
/// Every refusal is a 404, so that nothing tells what exists.
fn check_path(raw: &str, dotfiles: bool) -> Option<Relative> {
    if raw.len() > MAX_PATH {
        return None;
    }
    let parts: Vec<&str> = raw.split('/').collect();
    let mut segments = Vec::new();
    let mut directory = false;
    for (i, part) in parts.iter().enumerate() {
        let last = i + 1 == parts.len();
        if part.is_empty() {
            // `a/` names a directory; `a//b` is refused. An empty path is the root.
            if last {
                directory = true;
                continue;
            }
            return None;
        }
        // An encoded separator would let one segment hold two.
        let lower = part.to_ascii_lowercase();
        if lower.contains("%2f") || lower.contains("%5c") {
            return None;
        }
        let decoded = crate::http::percent_decode_path(part)?;
        if decoded.len() > MAX_SEGMENT || decoded.contains(['\0', '\\']) || decoded == ".." {
            return None;
        }
        if decoded == "." {
            directory |= last;
            continue;
        }
        if decoded.starts_with('.') && !dotfiles {
            return None;
        }
        segments.push(decoded);
    }
    Some(Relative { segments, directory })
}

/// `path`'s real location, if it is under `root` (already canonical).
fn inside(root: &Path, path: &Path) -> Option<PathBuf> {
    let real = std::fs::canonicalize(path).ok()?;
    real.starts_with(root).then_some(real)
}

/// Resolve a request for a file. `raw` is the path below the prefix as sent (without the
/// leading `/`), and `request_path` the whole path, for a redirect.
pub(crate) fn resolve(raw: &str, request_path: &str, options: &Options, conditions: &Conditions) -> Outcome {
    let Some(relative) = check_path(raw, options.dotfiles) else { return Outcome::NotFound };
    let Ok(root) = std::fs::canonicalize(options.root) else { return Outcome::NotFound };
    let mut path = root.clone();
    path.extend(&relative.segments);

    let mut fallback = false;
    let found = match std::fs::metadata(&path) {
        Ok(meta) if meta.is_dir() => {
            if !relative.directory && !relative.segments.is_empty() {
                // `/docs` names a directory: send the browser to `/docs/`, so that the index
                // page's relative links resolve inside it.
                let headers = format!("Location: {request_path}/\r\n");
                return Outcome::Respond { status: 301, content_type: "", headers, body: None };
            }
            if options.index.is_empty() {
                None
            } else {
                path.push(options.index);
                inside(&root, &path)
            }
        }
        Ok(_) if relative.directory => None,
        Ok(_) => inside(&root, &path),
        Err(_) => None,
    };
    let real = match found.filter(|p| p.is_file()) {
        Some(real) => real,
        None if !options.fallback.is_empty() => {
            fallback = true;
            match inside(&root, &root.join(options.fallback)).filter(|p| p.is_file()) {
                Some(real) => real,
                None => return Outcome::NotFound,
            }
        }
        None => return Outcome::NotFound,
    };
    let content_type = content_type(&real);

    // A pre-compressed copy beside the file, unless a range is asked for: ranges are of the
    // file as stored, and only the plain one is simple to reason about.
    let mut encoding = "";
    let mut variants = false;
    let mut chosen = real.clone();
    for (name, extension) in [("br", "br"), ("gzip", "gz")] {
        let mut candidate = real.clone().into_os_string();
        candidate.push(".");
        candidate.push(extension);
        let Some(candidate) = inside(&root, Path::new(&candidate)).filter(|p| p.is_file()) else { continue };
        variants = true;
        if encoding.is_empty() && conditions.range.is_none() && accepts_encoding(conditions.accept_encoding, name) {
            encoding = name;
            chosen = candidate;
        }
    }

    let Ok(file) = File::open(&chosen) else { return Outcome::NotFound };
    let Ok(meta) = file.metadata() else { return Outcome::NotFound };
    let size = meta.len();
    let (seconds, nanos) = modified(&meta);
    let etag = format!(
        "\"{size:x}-{:x}{}{encoding}\"",
        u128::from(seconds) * 1_000_000_000 + u128::from(nanos),
        if encoding.is_empty() { "" } else { "-" }
    );
    let last_modified = crate::http::format_http_date(seconds);

    let mut headers = String::new();
    headers.push_str("ETag: ");
    headers.push_str(&etag);
    headers.push_str("\r\nLast-Modified: ");
    headers.push_str(&last_modified);
    headers.push_str("\r\nCache-Control: ");
    if fallback {
        // A single-page app's page stands for many paths and changes with each release.
        headers.push_str("no-cache");
    } else {
        headers.push_str(&format!("public, max-age={}", options.max_age.max(0)));
        if options.immutable {
            headers.push_str(", immutable");
        }
    }
    headers.push_str("\r\n");
    if variants {
        headers.push_str("Vary: Accept-Encoding\r\n");
    }
    headers.push_str("X-Content-Type-Options: nosniff\r\n");

    // The preconditions, in the order of RFC 9110, section 13.2.2.
    if let Some(wanted) = conditions.if_match {
        if !matches_etag(wanted, &etag, false) {
            return Outcome::Respond { status: 412, content_type: "", headers, body: None };
        }
    } else if let Some(date) = conditions.if_unmodified_since.and_then(parse_http_date) {
        if seconds > date {
            return Outcome::Respond { status: 412, content_type: "", headers, body: None };
        }
    }
    if let Some(wanted) = conditions.if_none_match {
        if matches_etag(wanted, &etag, true) {
            return Outcome::Respond { status: 304, content_type: "", headers, body: None };
        }
    } else if let Some(date) = conditions.if_modified_since.and_then(parse_http_date) {
        if seconds <= date {
            return Outcome::Respond { status: 304, content_type: "", headers, body: None };
        }
    }
    if !encoding.is_empty() {
        headers.push_str("Content-Encoding: ");
        headers.push_str(encoding);
        headers.push_str("\r\n");
    }
    headers.push_str("Accept-Ranges: bytes\r\n");

    let range = conditions.range.filter(|_| match conditions.if_range {
        None => true,
        // An entity tag must match strongly; a date must be exactly the file's.
        Some(validator) if validator.trim_start().starts_with(['"', 'W']) => matches_etag(validator, &etag, false),
        Some(date) => parse_http_date(date) == Some(seconds),
    });
    let (status, start, len) = match range.map(|r| parse_range(r, size)) {
        None | Some(RangeAnswer::Whole) => (200, 0, size),
        Some(RangeAnswer::Part(start, end)) => {
            headers.push_str(&format!("Content-Range: bytes {start}-{end}/{size}\r\n"));
            (206, start, end - start + 1)
        }
        Some(RangeAnswer::Unsatisfiable) => {
            headers.push_str(&format!("Content-Range: bytes */{size}\r\n"));
            return Outcome::Respond { status: 416, content_type: "", headers, body: None };
        }
    };
    Outcome::Respond { status, content_type, headers, body: Some(Part { file, start, len }) }
}

/// A file's modification time, as seconds and nanoseconds since 1970.
fn modified(meta: &std::fs::Metadata) -> (u64, u32) {
    match meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()) {
        Some(d) => (d.as_secs(), d.subsec_nanos()),
        None => (0, 0),
    }
}

/// Whether an `If-Match` or `If-None-Match` list holds `etag` (`*` holds any). `weak`
/// comparison ignores `W/`; strong comparison never matches a weak tag.
fn matches_etag(list: &str, etag: &str, weak: bool) -> bool {
    list.split(',').map(str::trim).any(|tag| {
        if tag == "*" {
            return true;
        }
        match tag.strip_prefix("W/") {
            Some(bare) => weak && bare == etag,
            None => tag == etag,
        }
    })
}

/// Whether `Accept-Encoding` allows `name` (with a q-value above 0, or through `*`).
pub(crate) fn accepts_encoding(header: Option<&str>, name: &str) -> bool {
    let Some(header) = header else { return false };
    let mut star = None;
    for item in header.split(',') {
        let mut parts = item.split(';');
        let coding = parts.next().unwrap_or("").trim();
        let q = parts
            .filter_map(|p| p.trim().strip_prefix("q=").or_else(|| p.trim().strip_prefix("Q=")))
            .find_map(|v| v.trim().parse::<f64>().ok())
            .unwrap_or(1.0);
        if coding.eq_ignore_ascii_case(name) {
            return q > 0.0;
        }
        if coding == "*" {
            star = Some(q > 0.0);
        }
    }
    star.unwrap_or(false)
}

enum RangeAnswer {
    /// Send the whole file: no usable range (several ranges, another unit, bad syntax).
    Whole,
    /// Bytes `start` to `end`, inclusive.
    Part(u64, u64),
    Unsatisfiable,
}

/// One `bytes=a-b`, `a-` or `-n` range of a file of `size` bytes.
fn parse_range(header: &str, size: u64) -> RangeAnswer {
    let header = header.trim();
    let Some(spec) = header.get(..6).filter(|unit| unit.eq_ignore_ascii_case("bytes=")).map(|_| &header[6..]) else {
        return RangeAnswer::Whole;
    };
    if spec.contains(',') {
        return RangeAnswer::Whole;
    }
    let Some((first, last)) = spec.trim().split_once('-') else { return RangeAnswer::Whole };
    let number = |text: &str| -> Option<u64> {
        if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        text.parse().ok()
    };
    let (first, last) = (first.trim(), last.trim());
    if first.is_empty() {
        // The last `n` bytes.
        return match number(last) {
            None => RangeAnswer::Whole,
            Some(0) => RangeAnswer::Unsatisfiable,
            Some(_) if size == 0 => RangeAnswer::Unsatisfiable,
            Some(n) => RangeAnswer::Part(size.saturating_sub(n), size - 1),
        };
    }
    let Some(start) = number(first) else { return RangeAnswer::Whole };
    let end = if last.is_empty() {
        u64::MAX
    } else {
        match number(last) {
            Some(end) if end >= start => end,
            _ => return RangeAnswer::Whole,
        }
    };
    if start >= size {
        return RangeAnswer::Unsatisfiable;
    }
    RangeAnswer::Part(start, end.min(size - 1))
}

/// An IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`) as seconds since 1970. Other forms are
/// not understood, and a header that is not a date is ignored, as RFC 9110 asks.
pub(crate) fn parse_http_date(text: &str) -> Option<u64> {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let text = text.trim();
    let (_, rest) = text.split_once(", ")?;
    let mut parts = rest.split(' ');
    let day: u32 = parts.next()?.parse().ok()?;
    let month_name = parts.next()?;
    let month = MONTHS.iter().position(|m| *m == month_name)? as u32 + 1;
    let year: i64 = parts.next()?.parse().ok()?;
    let mut clock = parts.next()?.split(':');
    let (h, m, s): (u64, u64, u64) =
        (clock.next()?.parse().ok()?, clock.next()?.parse().ok()?, clock.next()?.parse().ok()?);
    if parts.next()? != "GMT" || parts.next().is_some() || !(1..=31).contains(&day) || h > 23 || m > 59 || s > 60 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    (days >= 0).then(|| days as u64 * 86_400 + h * 3600 + m * 60 + s)
}

/// Days since 1970-01-01 of a civil date (Howard Hinnant's algorithm; the inverse of
/// `http::civil`).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = i64::from((month + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The `Content-Type` for a file, from its extension.
fn content_type(path: &Path) -> &'static str {
    let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match extension.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" | "cjs" => "text/javascript; charset=utf-8",
        "json" | "map" => "application/json",
        "jsonld" => "application/ld+json",
        "webmanifest" => "application/manifest+json",
        "txt" | "text" => "text/plain; charset=utf-8",
        "md" | "markdown" => "text/markdown; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "tsv" => "text/tab-separated-values; charset=utf-8",
        "xml" => "application/xml",
        "rss" => "application/rss+xml",
        "atom" => "application/atom+xml",
        "yaml" | "yml" => "application/yaml",
        "toml" => "application/toml",
        "ics" => "text/calendar; charset=utf-8",
        "vtt" => "text/vtt; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "eot" => "application/vnd.ms-fontobject",
        "pdf" => "application/pdf",
        "wasm" => "application/wasm",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "ogv" => "video/ogg",
        "mov" => "video/quicktime",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "oga" | "ogg" | "opus" => "audio/ogg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "mid" | "midi" => "audio/midi",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "br" => "application/x-brotli",
        "tar" => "application/x-tar",
        "bz2" => "application/x-bzip2",
        "xz" => "application/x-xz",
        "7z" => "application/x-7z-compressed",
        "rar" => "application/vnd.rar",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "odt" => "application/vnd.oasis.opendocument.text",
        "epub" => "application/epub+zip",
        "rtf" => "application/rtf",
        "apk" => "application/vnd.android.package-archive",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_checked_segment_by_segment() {
        let ok = |raw: &str| check_path(raw, false).map(|r| (r.segments.join("/"), r.directory));
        assert_eq!(ok("css/site.css"), Some(("css/site.css".into(), false)));
        assert_eq!(ok(""), Some(("".into(), true)));
        assert_eq!(ok("docs/"), Some(("docs".into(), true)));
        assert_eq!(ok("a/./b"), Some(("a/b".into(), false)));
        assert_eq!(ok("caf%C3%A9.txt"), Some(("café.txt".into(), false)));
        for refused in [
            "../etc/passwd",
            "a/../../b",
            "%2e%2e/secret",
            "a/%2E%2E/b",
            "a%2Fb",
            "a%2fb",
            "..%5Cwindows",
            "a%5cb",
            "a\\b",
            "a%00b",
            "a//b",
            ".env",
            "a/.git/config",
            "%2eenv",
            "bad%zz",
        ] {
            assert_eq!(ok(refused), None, "{refused}");
        }
        assert!(check_path(".well-known/x", true).is_some());
        assert!(check_path(&"a/".repeat(3000), false).is_none());
        assert!(check_path(&"a".repeat(300), false).is_none());
    }

    #[test]
    fn ranges() {
        let r = |h: &str| match parse_range(h, 100) {
            RangeAnswer::Whole => "whole".to_string(),
            RangeAnswer::Part(a, b) => format!("{a}-{b}"),
            RangeAnswer::Unsatisfiable => "416".to_string(),
        };
        assert_eq!(r("bytes=0-9"), "0-9");
        assert_eq!(r("bytes=90-"), "90-99");
        assert_eq!(r("bytes=90-500"), "90-99");
        assert_eq!(r("bytes=-10"), "90-99");
        assert_eq!(r("bytes=-500"), "0-99");
        assert_eq!(r("Bytes=5-5"), "5-5");
        assert_eq!(r("bytes=100-"), "416");
        assert_eq!(r("bytes=-0"), "416");
        assert_eq!(r("bytes=0-1,5-6"), "whole");
        assert_eq!(r("items=0-1"), "whole");
        assert_eq!(r("bytes=9-3"), "whole");
        assert_eq!(r("bytes=x-3"), "whole");
    }

    #[test]
    fn validators_and_encodings() {
        let tag = "\"10-20\"";
        assert!(matches_etag("\"10-20\"", tag, false));
        assert!(matches_etag("\"x\", \"10-20\"", tag, true));
        assert!(matches_etag("W/\"10-20\"", tag, true));
        assert!(!matches_etag("W/\"10-20\"", tag, false));
        assert!(matches_etag("*", tag, false));
        assert!(!matches_etag("\"10-21\"", tag, true));
        assert!(accepts_encoding(Some("gzip, deflate, br"), "br"));
        assert!(!accepts_encoding(Some("gzip;q=0, br;q=0"), "gzip"));
        assert!(accepts_encoding(Some("*"), "gzip"));
        assert!(!accepts_encoding(Some("*, gzip;q=0"), "gzip"));
        assert!(!accepts_encoding(None, "gzip"));
    }

    #[test]
    fn http_dates_round_trip() {
        assert_eq!(parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT"), Some(784_111_777));
        for seconds in [0, 784_111_777, 1_795_000_000, 4_102_444_800] {
            assert_eq!(parse_http_date(&crate::http::format_http_date(seconds)), Some(seconds));
        }
        assert_eq!(parse_http_date("Sunday, 06-Nov-94 08:49:37 GMT"), None);
        assert_eq!(parse_http_date("yesterday"), None);
    }
}
