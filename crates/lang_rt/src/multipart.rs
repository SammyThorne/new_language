//! Form bodies: `application/x-www-form-urlencoded` and `multipart/form-data` (RFC 7578), for
//! `req.form()` and the `http.Form[T]` and `http.Upload` handler parameters
//! (docs/05-standard-library.md, "Forms and uploads").
//!
//! The body is read whole first, under the route's `max_body` (docs/14, milestone 11), and then
//! split here. Text fields are copied out when the program asks for them; a file part larger
//! than [`SPILL_AT`] is written to a temporary file in the server's `upload_dir`, which is
//! removed when the request ends unless `Upload.save` moved it.
//!
//! The parser accepts what browsers and HTTP libraries send, and a little more: lines may end
//! in a lone LF as well as CRLF, a boundary may have transport padding after it, and a file
//! name may be given as `filename*` (RFC 5987), which wins over `filename`. A part sent for an
//! empty file input (`filename=""` and no data) is left out.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};

use crate::Str;

/// File parts larger than this go to a temporary file rather than collected memory.
pub(crate) const SPILL_AT: usize = 1024 * 1024;
/// RFC 2046's limit on a boundary's length.
const MAX_BOUNDARY: usize = 70;

static MAX_FILES: AtomicI64 = AtomicI64::new(10);
static MAX_FIELDS: AtomicI64 = AtomicI64::new(1000);
static MAX_PART_HEADERS: AtomicI64 = AtomicI64::new(8192);
/// Where large file parts are written: `server.upload_dir`, or the system's temporary
/// directory when that is `""`.
static UPLOAD_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// The limits on a form: `server.max_files`, `max_fields`, `max_part_headers` and `upload_dir`.
///
/// # Safety
/// `ptr` and `len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_http_set_form_limits(
    files: i64,
    fields: i64,
    part_headers: i64,
    ptr: *const u8,
    len: i64,
) {
    MAX_FILES.store(files.max(0), Ordering::Relaxed);
    MAX_FIELDS.store(fields.max(0), Ordering::Relaxed);
    MAX_PART_HEADERS.store(part_headers.max(64), Ordering::Relaxed);
    let dir = unsafe { Str::from_raw(ptr, len) };
    *UPLOAD_DIR.lock().unwrap_or_else(|p| p.into_inner()) = (!dir.is_empty()).then(|| PathBuf::from(dir));
}

/// The limits a form is parsed under.
pub(crate) struct Limits {
    pub files: usize,
    pub fields: usize,
    pub part_headers: usize,
    /// Where to write file parts larger than [`SPILL_AT`]; `None` keeps everything in memory
    /// (test requests).
    pub spill_to: Option<PathBuf>,
}

impl Limits {
    /// The server's.
    pub(crate) fn server() -> Limits {
        let dir = UPLOAD_DIR.lock().unwrap_or_else(|p| p.into_inner()).clone();
        Limits {
            files: MAX_FILES.load(Ordering::Relaxed) as usize,
            fields: MAX_FIELDS.load(Ordering::Relaxed) as usize,
            part_headers: MAX_PART_HEADERS.load(Ordering::Relaxed) as usize,
            spill_to: Some(dir.unwrap_or_else(std::env::temp_dir)),
        }
    }
}

/// Where a part's data is.
#[derive(Debug, PartialEq)]
pub(crate) enum Data {
    /// A range of the body.
    Body(usize, usize),
    /// Bytes decoded from the body (urlencoded values).
    Owned(Vec<u8>),
    /// A temporary file, and its size.
    File(PathBuf, usize),
}

#[derive(Debug, PartialEq)]
pub(crate) struct Part {
    pub name: String,
    /// `Some` for a file, sanitised: the base name only, without control characters.
    pub filename: Option<String>,
    pub content_type: String,
    pub data: Data,
}

impl Part {
    fn size(&self) -> usize {
        match &self.data {
            Data::Body(start, end) => end - start,
            Data::Owned(bytes) => bytes.len(),
            Data::File(_, size) => *size,
        }
    }
}

/// A parsed form, and the body its parts point into.
pub(crate) struct Form {
    body: Body,
    pub parts: Vec<Part>,
}

enum Body {
    /// The request's body, which outlives the form: the form is freed before the handler
    /// returns.
    Borrowed(*const u8, usize),
    Owned(Vec<u8>),
}

impl Form {
    fn body(&self) -> &[u8] {
        match &self.body {
            // SAFETY: see `Body::Borrowed`.
            Body::Borrowed(ptr, len) => unsafe { std::slice::from_raw_parts(*ptr, *len) },
            Body::Owned(bytes) => bytes,
        }
    }

    fn data<'a>(&'a self, part: &'a Part) -> &'a [u8] {
        match &part.data {
            Data::Body(start, end) => &self.body()[*start..*end],
            Data::Owned(bytes) => bytes,
            Data::File(..) => &[],
        }
    }
}

/// Why a form could not be read: the status to answer and the message.
pub(crate) type Refusal = (u16, String);

fn bad(message: impl Into<String>) -> Refusal {
    (400, message.into())
}

/// Split a body sent with this `Content-Type`. Temporary files written for large parts are
/// added to `temp` (also when a later part fails), for the caller to remove.
pub(crate) fn parse(
    content_type: &str,
    body: &[u8],
    limits: &Limits,
    temp: &mut Vec<PathBuf>,
) -> Result<Vec<Part>, Refusal> {
    let (essence, params) = content_type.split_once(';').unwrap_or((content_type, ""));
    let essence = essence.trim();
    if essence.eq_ignore_ascii_case("application/x-www-form-urlencoded") {
        return urlencoded(body, limits);
    }
    if essence.eq_ignore_ascii_case("multipart/form-data") {
        let boundary = parameters(params)
            .into_iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("boundary"))
            .map(|(_, value)| value)
            .ok_or_else(|| bad("the multipart body has no boundary"))?;
        if boundary.is_empty() || boundary.len() > MAX_BOUNDARY {
            return Err(bad(format!("the multipart boundary must be 1 to {MAX_BOUNDARY} characters long")));
        }
        return multipart(body, boundary.as_bytes(), limits, temp);
    }
    Err((415, "a form must be sent as application/x-www-form-urlencoded or multipart/form-data".to_string()))
}

fn urlencoded(body: &[u8], limits: &Limits) -> Result<Vec<Part>, Refusal> {
    let text = std::str::from_utf8(body).map_err(|_| bad("the form is not UTF-8 text"))?;
    let mut parts = Vec::new();
    for pair in text.split('&').filter(|p| !p.is_empty()) {
        if parts.len() == limits.fields {
            return Err((413, format!("the form has more than {} fields", limits.fields)));
        }
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        let decode = |text: &str| {
            crate::http::percent_decode(text, true).ok_or_else(|| bad("the form has a malformed `%` escape"))
        };
        parts.push(Part {
            name: decode(name)?,
            filename: None,
            content_type: "text/plain".to_string(),
            data: Data::Owned(decode(value)?.into_bytes()),
        });
    }
    Ok(parts)
}

/// Where the next line starting with `--boundary` begins, at or after `from`, and where the
/// delimiter's line ends (past its CRLF or LF), or `None` for the closing delimiter (`--` after
/// it). A line that only starts with the delimiter (`--boundaryX`) is data.
fn find_delimiter(body: &[u8], from: usize, boundary: &[u8]) -> Result<(usize, Option<usize>), Refusal> {
    let mut at = from;
    loop {
        let start = if at == 0 && body.starts_with(b"--") {
            0
        } else {
            let newline = body[at..]
                .iter()
                .position(|&b| b == b'\n')
                .ok_or_else(|| bad("the multipart body ends without its closing boundary"))?;
            at + newline + 1
        };
        let rest = &body[start..];
        if rest.len() >= 2 + boundary.len() && rest.starts_with(b"--") && &rest[2..2 + boundary.len()] == boundary {
            let after = &rest[2 + boundary.len()..];
            if after.starts_with(b"--") {
                return Ok((start, None));
            }
            // Transport padding, then the line's end.
            let padding = after.iter().take_while(|&&b| b == b' ' || b == b'\t').count();
            let tail = &after[padding..];
            let line_end = if tail.starts_with(b"\r\n") {
                Some(2)
            } else if tail.starts_with(b"\n") {
                Some(1)
            } else {
                None
            };
            if let Some(n) = line_end {
                return Ok((start, Some(start + 2 + boundary.len() + padding + n)));
            }
        }
        if start == 0 {
            // A body starting with `--` that is not the delimiter: look from the next line.
            let newline =
                body.iter().position(|&b| b == b'\n').ok_or_else(|| bad("the multipart body has no boundary line"))?;
            at = newline;
            if at + 1 >= body.len() {
                return Err(bad("the multipart body has no boundary line"));
            }
            continue;
        }
        at = start;
    }
}

fn multipart(body: &[u8], boundary: &[u8], limits: &Limits, temp: &mut Vec<PathBuf>) -> Result<Vec<Part>, Refusal> {
    let mut parts = Vec::new();
    let (mut files, mut fields) = (0, 0);
    // The preamble, before the first delimiter, is ignored.
    let (_, mut next) = find_delimiter(body, 0, boundary)?;
    while let Some(start) = next {
        // The part's headers, up to an empty line.
        let mut at = start;
        let mut disposition = None;
        let mut content_type = None;
        loop {
            let newline =
                body[at..].iter().position(|&b| b == b'\n').ok_or_else(|| bad("a part's headers have no end"))?;
            let line = &body[at..at + newline];
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            at += newline + 1;
            if at - start > limits.part_headers {
                return Err(bad(format!("a part's headers are longer than {} bytes", limits.part_headers)));
            }
            if line.is_empty() {
                break;
            }
            let line = std::str::from_utf8(line).map_err(|_| bad("a part's headers are not UTF-8 text"))?;
            let Some((name, value)) = line.split_once(':') else {
                return Err(bad(format!("a part has a malformed header line: {line:?}")));
            };
            if name.trim().eq_ignore_ascii_case("content-disposition") {
                disposition = Some(value.trim().to_string());
            } else if name.trim().eq_ignore_ascii_case("content-type") {
                content_type = Some(value.trim().to_string());
            }
        }
        // The data, up to the line break before the next delimiter.
        // Searching from the blank line's own LF also finds a delimiter straight after it, for
        // an empty part sent without the line break before the delimiter.
        let (delimiter, after) = find_delimiter(body, at - 1, boundary)?;
        let mut end = delimiter.saturating_sub(1);
        if end > at && body[end - 1] == b'\r' {
            end -= 1;
        }
        let end = end.max(at);
        next = after;

        let disposition = disposition.ok_or_else(|| bad("a part has no Content-Disposition header"))?;
        let (kind, params) = disposition.split_once(';').unwrap_or((&disposition, ""));
        if !kind.trim().eq_ignore_ascii_case("form-data") {
            return Err(bad(format!("a part's Content-Disposition is `{}`, not `form-data`", kind.trim())));
        }
        let mut name = None;
        let mut filename = None;
        let mut extended = None;
        for (key, value) in parameters(params) {
            if key.eq_ignore_ascii_case("name") {
                name = Some(value);
            } else if key.eq_ignore_ascii_case("filename") {
                filename = Some(value);
            } else if key.eq_ignore_ascii_case("filename*") {
                extended = decode_extended(&value);
            }
        }
        let name = name.ok_or_else(|| bad("a part has no name"))?;
        let filename = extended.or(filename).map(|f| sanitise(&f));
        match filename {
            Some(filename) => {
                if filename.is_empty() && end == at {
                    // A file input left empty.
                    continue;
                }
                files += 1;
                if files > limits.files {
                    return Err((413, format!("the form has more than {} files", limits.files)));
                }
                let content_type = content_type.unwrap_or_else(|| "application/octet-stream".to_string());
                let data = match &limits.spill_to {
                    Some(dir) if end - at > SPILL_AT => {
                        let path = spill(dir, &body[at..end])
                            .map_err(|e| (500, format!("could not write an upload to {}: {e}", dir.display())))?;
                        temp.push(path.clone());
                        Data::File(path, end - at)
                    }
                    _ => Data::Body(at, end),
                };
                parts.push(Part { name, filename: Some(filename), content_type, data });
            }
            None => {
                fields += 1;
                if fields > limits.fields {
                    return Err((413, format!("the form has more than {} fields", limits.fields)));
                }
                if std::str::from_utf8(&body[at..end]).is_err() {
                    return Err(bad(format!("form field `{name}` is not UTF-8 text")));
                }
                let content_type = content_type.unwrap_or_else(|| "text/plain".to_string());
                parts.push(Part { name, filename: None, content_type, data: Data::Body(at, end) });
            }
        }
    }
    Ok(parts)
}

/// `; name=value; name="quoted \" value"` as pairs, names trimmed.
fn parameters(text: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    let mut chars = text.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| *c == ';' || c.is_whitespace()) {
            chars.next();
        }
        let mut name = String::new();
        while let Some(&c) = chars.peek() {
            if c == '=' || c == ';' {
                break;
            }
            name.push(c);
            chars.next();
        }
        if name.is_empty() && chars.peek().is_none() {
            return pairs;
        }
        let mut value = String::new();
        if chars.peek() == Some(&'=') {
            chars.next();
            while chars.peek().is_some_and(|c| *c == ' ' || *c == '\t') {
                chars.next();
            }
            if chars.peek() == Some(&'"') {
                chars.next();
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        // An escape only before `"` or `\`: some browsers send a Windows path
                        // with its backslashes as they are.
                        '\\' if matches!(chars.peek(), Some('"' | '\\')) => value.extend(chars.next()),
                        c => value.push(c),
                    }
                }
                // Anything after the closing quote, up to the next `;`, is ignored.
                while chars.peek().is_some_and(|c| *c != ';') {
                    chars.next();
                }
            } else {
                while let Some(&c) = chars.peek() {
                    if c == ';' {
                        break;
                    }
                    value.push(c);
                    chars.next();
                }
                value = value.trim_end().to_string();
            }
        }
        pairs.push((name.trim().to_string(), value));
    }
}

/// An RFC 5987 value, `UTF-8'en'na%C3%AFve.txt`: UTF-8 or ISO-8859-1; `None` if malformed.
fn decode_extended(value: &str) -> Option<String> {
    let mut pieces = value.splitn(3, '\'');
    let charset = pieces.next()?;
    let _language = pieces.next()?;
    let encoded = pieces.next()?;
    let mut bytes = Vec::with_capacity(encoded.len());
    let raw = encoded.as_bytes();
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'%' {
            let hex = std::str::from_utf8(raw.get(i + 1..i + 3)?).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            bytes.push(raw[i]);
            i += 1;
        }
    }
    if charset.eq_ignore_ascii_case("utf-8") {
        String::from_utf8(bytes).ok()
    } else if charset.eq_ignore_ascii_case("iso-8859-1") {
        Some(bytes.iter().map(|&b| char::from(b)).collect())
    } else {
        None
    }
}

/// The base name only: what follows the last `/` or `\`, without control characters, and not
/// `.` or `..`.
pub(crate) fn sanitise(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let clean: String = base.chars().filter(|c| !c.is_control()).collect();
    let clean = clean.trim().to_string();
    if clean == "." || clean == ".." { String::new() } else { clean }
}

/// Write a large part to a new file in `dir`, readable only by this user.
fn spill(dir: &Path, data: &[u8]) -> std::io::Result<PathBuf> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut random = [0u8; 12];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut random)
        .map_err(|_| std::io::Error::other("no random numbers"))?;
    let name: String = random.iter().map(|b| format!("{b:02x}")).collect();
    let path = dir.join(format!("upload-{name}"));
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)?;
    if let Err(error) = file.write_all(data) {
        let _ = std::fs::remove_file(&path);
        return Err(error);
    }
    Ok(path)
}

/// Temporary files written for a request's uploads, removed when it ends: those `save` moved
/// are gone already.
#[derive(Default)]
pub(crate) struct TempFiles(pub Vec<PathBuf>);

impl Drop for TempFiles {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

// ---- externs ------------------------------------------------------------------------------

fn refused((status, message): Refusal) -> i64 {
    crate::sys::record_error(message);
    -i64::from(status)
}

/// Parse a test request's body, kept in memory: a form handle, or minus the status to answer
/// with the reason as the last error.
///
/// # Safety
/// The pointers and lengths describe strings; `content_type` is UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_http_parse_form(
    type_ptr: *const u8,
    type_len: i64,
    body_ptr: *const u8,
    body_len: i64,
) -> i64 {
    let content_type = unsafe { Str::from_raw(type_ptr, type_len) };
    let body = if body_len <= 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(body_ptr, body_len as usize) }.to_vec()
    };
    let mut limits = Limits::server();
    limits.spill_to = None;
    match parse(content_type, &body, &limits, &mut Vec::new()) {
        Ok(parts) => Box::into_raw(Box::new(Form { body: Body::Owned(body), parts })) as i64,
        Err(refusal) => refused(refusal),
    }
}

/// Parse a live request's body (`req.form()`), reading it first if it is pending.
pub(crate) fn request_form(content_type: &str, body: &[u8], temp: &mut Vec<PathBuf>) -> i64 {
    match parse(content_type, body, &Limits::server(), temp) {
        Ok(parts) => Box::into_raw(Box::new(Form { body: Body::Borrowed(body.as_ptr(), body.len()), parts })) as i64,
        Err(refusal) => refused(refusal),
    }
}

/// # Safety
/// `form` comes from a form parse and has not been freed.
unsafe fn form<'a>(form: i64) -> &'a Form {
    unsafe { &*(form as *const Form) }
}

/// How many parts the form has.
///
/// # Safety
/// `handle` is a live form.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_http_form_count(handle: i64) -> i64 {
    unsafe { form(handle) }.parts.len() as i64
}

/// Part `i`'s name (`what` 0), content type (2), data (3; empty when it is in a file) or
/// temporary file's path (4; `""` when it is in memory).
///
/// # Safety
/// `handle` is a live form.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_http_form_part(handle: i64, i: i64, what: i64) -> Str {
    let form = unsafe { form(handle) };
    let Some(part) = usize::try_from(i).ok().and_then(|i| form.parts.get(i)) else { return Str::from_bytes(b"") };
    match what {
        0 => Str::from_bytes(part.name.as_bytes()),
        2 => Str::from_bytes(part.content_type.as_bytes()),
        3 => Str::from_bytes(form.data(part)),
        _ => match &part.data {
            Data::File(path, _) => Str::from_bytes(path.to_string_lossy().as_bytes()),
            _ => Str::from_bytes(b""),
        },
    }
}

/// Part `i`'s file name, or `None` for a text field.
///
/// # Safety
/// `handle` is a live form.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_http_form_filename(handle: i64, i: i64) -> Str {
    let form = unsafe { form(handle) };
    crate::http::optional(usize::try_from(i).ok().and_then(|i| form.parts.get(i)).and_then(|p| p.filename.as_deref()))
}

/// Part `i`'s size in bytes.
///
/// # Safety
/// `handle` is a live form.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_http_form_size(handle: i64, i: i64) -> i64 {
    let form = unsafe { form(handle) };
    usize::try_from(i).ok().and_then(|i| form.parts.get(i)).map_or(0, |p| p.size() as i64)
}

/// # Safety
/// `handle` is a live form, not used again.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_http_form_free(handle: i64) {
    drop(unsafe { Box::from_raw(handle as *mut Form) });
}

/// A file name as an upload would get it: the base name, without control characters.
///
/// # Safety
/// `ptr` and `len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_http_sanitise_filename(ptr: *const u8, len: i64) -> Str {
    Str::from_bytes(sanitise(unsafe { Str::from_raw(ptr, len) }).as_bytes())
}

/// An upload's contents from its temporary file; empty, with the last error set, if it is gone
/// (the request has ended).
///
/// # Safety
/// `ptr` and `len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_http_upload_read(ptr: *const u8, len: i64) -> Str {
    let path = unsafe { Str::from_raw(ptr, len) };
    match std::fs::read(path) {
        Ok(bytes) => Str::from_bytes(&bytes),
        Err(error) => {
            crate::sys::record_error(format!("could not read the upload: {error}"));
            Str::from_bytes(b"")
        }
    }
}

/// Keep an upload at `dest`: its temporary file (`path`) is moved there, or copied when it is
/// on another file system; otherwise `data` is written. 0, or -1 with the last error set.
///
/// # Safety
/// The pointers and lengths describe strings; `path` and `dest` are UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_http_upload_save(
    path_ptr: *const u8,
    path_len: i64,
    data_ptr: *const u8,
    data_len: i64,
    dest_ptr: *const u8,
    dest_len: i64,
) -> i64 {
    let path = unsafe { Str::from_raw(path_ptr, path_len) };
    let dest = unsafe { Str::from_raw(dest_ptr, dest_len) };
    let result = if path.is_empty() {
        let data =
            if data_len <= 0 { &[][..] } else { unsafe { std::slice::from_raw_parts(data_ptr, data_len as usize) } };
        std::fs::write(dest, data)
    } else {
        std::fs::rename(path, dest).or_else(|_| std::fs::copy(path, dest).map(|_| ()))
    };
    match result {
        Ok(()) => 0,
        Err(error) => {
            crate::sys::record_error(format!("could not save the upload to {dest}: {error}"));
            -1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> Limits {
        Limits { files: 10, fields: 1000, part_headers: 8192, spill_to: None }
    }

    /// A part as (name, file name, content type, data).
    type Parsed = (String, Option<String>, String, Vec<u8>);

    fn form(content_type: &str, body: &[u8]) -> Result<Vec<Parsed>, Refusal> {
        let parts = parse(content_type, body, &limits(), &mut Vec::new())?;
        Ok(parts
            .into_iter()
            .map(|p| {
                let data = match &p.data {
                    Data::Body(s, e) => body[*s..*e].to_vec(),
                    Data::Owned(b) => b.clone(),
                    Data::File(..) => panic!("spilled"),
                };
                (p.name, p.filename, p.content_type, data)
            })
            .collect())
    }

    const TYPE: &str = "multipart/form-data; boundary=XyZ";

    #[test]
    fn urlencoded_fields() {
        let parts = form("application/x-www-form-urlencoded", b"name=Ada+L&tag=a%26b&tag=&empty").unwrap();
        let fields: Vec<_> = parts.iter().map(|p| (p.0.as_str(), String::from_utf8(p.3.clone()).unwrap())).collect();
        assert_eq!(fields, [("name", "Ada L".into()), ("tag", "a&b".into()), ("tag", "".into()), ("empty", "".into())]);
        assert_eq!(form("application/x-www-form-urlencoded", b"a=%zz").unwrap_err().0, 400);
    }

    #[test]
    fn crlf_and_lone_lf() {
        let crlf = b"--XyZ\r\nContent-Disposition: form-data; name=\"title\"\r\n\r\nhello\r\n--XyZ\r\nContent-Disposition: form-data; name=\"f\"; filename=\"a.txt\"\r\nContent-Type: text/plain\r\n\r\nline1\r\nline2\r\n--XyZ--\r\n";
        let parts = form(TYPE, crlf).unwrap();
        assert_eq!(parts[0], ("title".into(), None, "text/plain".into(), b"hello".to_vec()));
        assert_eq!(parts[1], ("f".into(), Some("a.txt".into()), "text/plain".into(), b"line1\r\nline2".to_vec()));
        let lf = b"--XyZ\nContent-Disposition: form-data; name=\"title\"\n\nhello\n--XyZ--\n";
        assert_eq!(form(TYPE, lf).unwrap()[0].3, b"hello");
    }

    #[test]
    fn a_boundary_inside_the_data_is_data() {
        let body = b"preamble\r\n--XyZ\r\nContent-Disposition: form-data; name=\"f\"; filename=\"x\"\r\n\r\n--XyZz\r\n--XyZ-\r\nabc--XyZ\r\n--XyZ--";
        let parts = form(TYPE, body).unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].3, b"--XyZz\r\n--XyZ-\r\nabc--XyZ");
    }

    #[test]
    fn filename_star_wins_and_names_are_sanitised() {
        let body = "--XyZ\r\nContent-Disposition: form-data; name=\"f\"; filename=\"fallback.txt\"; filename*=UTF-8''na%C3%AFve%20file.txt\r\n\r\nx\r\n--XyZ\r\nContent-Disposition: form-data; name=\"g\"; filename=\"C:\\\\Users\\\\me\\\\..\\\\evil\x07.png\"\r\n\r\ny\r\n--XyZ\r\nContent-Disposition: form-data; name=\"h\"; filename=\"../../etc/passwd\"\r\n\r\nz\r\n--XyZ--\r\n";
        let parts = form(TYPE, body.as_bytes()).unwrap();
        assert_eq!(parts[0].1.as_deref(), Some("naïve file.txt"));
        assert_eq!(parts[1].1.as_deref(), Some("evil.png"));
        assert_eq!(parts[2].1.as_deref(), Some("passwd"));
        assert_eq!(parts[1].2, "application/octet-stream");
        assert_eq!(decode_extended("iso-8859-1'en'caf%E9").as_deref(), Some("café"));
        let windows =
            "--XyZ\r\nContent-Disposition: form-data; name=\"f\"; filename=\"C:\\photos\\x.png\"\r\n\r\nx\r\n--XyZ--";
        assert_eq!(form(TYPE, windows.as_bytes()).unwrap()[0].1.as_deref(), Some("x.png"));
    }

    #[test]
    fn empty_parts() {
        let body = b"--XyZ\r\nContent-Disposition: form-data; name=\"note\"\r\n\r\n\r\n--XyZ\r\nContent-Disposition: form-data; name=\"file\"; filename=\"\"\r\nContent-Type: application/octet-stream\r\n\r\n\r\n--XyZ\r\nContent-Disposition: form-data; name=\"empty\"; filename=\"empty.txt\"\r\n\r\n\r\n--XyZ--\r\n";
        let parts = form(TYPE, body).unwrap();
        // The empty field stays; the unselected file input goes; an empty file stays.
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], ("note".into(), None, "text/plain".into(), Vec::new()));
        assert_eq!(parts[1].1.as_deref(), Some("empty.txt"));
        assert!(parts[1].3.is_empty());
    }

    #[test]
    fn malformed_bodies() {
        let no_end = b"--XyZ\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\nvalue";
        assert_eq!(form(TYPE, no_end).unwrap_err(), bad("the multipart body ends without its closing boundary"));
        let no_name = b"--XyZ\r\nContent-Disposition: form-data\r\n\r\nv\r\n--XyZ--";
        assert_eq!(form(TYPE, no_name).unwrap_err(), bad("a part has no name"));
        let no_disposition = b"--XyZ\r\nContent-Type: text/plain\r\n\r\nv\r\n--XyZ--";
        assert_eq!(form(TYPE, no_disposition).unwrap_err().1, "a part has no Content-Disposition header");
        assert_eq!(form("multipart/form-data", b"").unwrap_err().1, "the multipart body has no boundary");
        let long = format!("multipart/form-data; boundary={}", "b".repeat(71));
        assert_eq!(form(&long, b"").unwrap_err().0, 400);
        assert_eq!(form("application/json", b"{}").unwrap_err().0, 415);
        let quoted = "multipart/form-data; boundary=\"a b\"";
        assert_eq!(
            form(quoted, b"--a b\r\nContent-Disposition: form-data; name=x\r\n\r\n1\r\n--a b--").unwrap()[0].3,
            b"1"
        );
        let not_utf8 = b"--XyZ\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\n\xff\r\n--XyZ--";
        assert_eq!(form(TYPE, not_utf8).unwrap_err().1, "form field `a` is not UTF-8 text");
    }

    #[test]
    fn limits_on_files_fields_and_headers() {
        let mut body = String::new();
        for i in 0..3 {
            body += &format!("--XyZ\r\nContent-Disposition: form-data; name=\"f\"; filename=\"{i}.txt\"\r\n\r\nx\r\n");
        }
        body += "--XyZ--\r\n";
        let tight = Limits { files: 2, ..limits() };
        assert_eq!(
            parse(TYPE, body.as_bytes(), &tight, &mut Vec::new()).unwrap_err(),
            (413, "the form has more than 2 files".into())
        );
        let tight = Limits { fields: 1, ..limits() };
        assert_eq!(parse("application/x-www-form-urlencoded", b"a=1&b=2", &tight, &mut Vec::new()).unwrap_err().0, 413);
        let long_header = format!(
            "--XyZ\r\nContent-Disposition: form-data; name=\"a\"\r\nX-Long: {}\r\n\r\nv\r\n--XyZ--",
            "h".repeat(200)
        );
        let tight = Limits { part_headers: 100, ..limits() };
        assert_eq!(
            parse(TYPE, long_header.as_bytes(), &tight, &mut Vec::new()).unwrap_err(),
            bad("a part's headers are longer than 100 bytes")
        );
    }

    #[test]
    fn large_parts_spill_and_are_removed() {
        let dir = std::env::temp_dir().join(format!("multipart-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let big = vec![b'a'; SPILL_AT + 1];
        let mut body = b"--XyZ\r\nContent-Disposition: form-data; name=\"f\"; filename=\"big.bin\"\r\n\r\n".to_vec();
        body.extend_from_slice(&big);
        body.extend_from_slice(b"\r\n--XyZ\r\nContent-Disposition: form-data; name=\"g\"; filename=\"small.bin\"\r\n\r\nsmall\r\n--XyZ--\r\n");
        let limits = Limits { spill_to: Some(dir.clone()), ..limits() };
        let mut temp = Vec::new();
        let parts = parse(TYPE, &body, &limits, &mut temp).unwrap();
        let Data::File(path, size) = &parts[0].data else { panic!("not spilled") };
        assert_eq!(*size, SPILL_AT + 1);
        assert_eq!(std::fs::read(path).unwrap(), big);
        assert!(matches!(parts[1].data, Data::Body(..)));
        assert_eq!(temp, std::slice::from_ref(path));
        drop(TempFiles(temp));
        assert!(!path.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
