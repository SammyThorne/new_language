//! An HTTP/1.1 client for `http.get`, `http.post` and `http.fetch` (docs/06, milestone 12).
//!
//! One request per connection (`Connection: close`), over the same non-blocking socket as the
//! server, so a task waiting for a response parks instead of blocking a worker. Response bodies
//! may be sized by `Content-Length`, chunked, or end with the connection. `https://` URLs use TLS
//! (see `tls.rs` for which certificates are trusted).
//!
//! The response of the last request on this thread is kept for the accessor functions, which the
//! standard library calls straight after the request.

use crate::http::Socket;
use crate::tls::{Protocol, Session};
use crate::{Str, sys};
use std::cell::RefCell;
use std::net::ToSocketAddrs;
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(30);
/// The most a gzip body may decode to: a small body must not become an unbounded one.
const MAX_DECODED: usize = 1 << 30;
const MAX_RESPONSE: usize = 64 * 1024 * 1024;

#[derive(Default)]
struct Response {
    headers: String,
    body: Vec<u8>,
}

thread_local! {
    static LAST: RefCell<Response> = RefCell::new(Response::default());
}

#[derive(Debug)]
struct Url {
    tls: bool,
    host: String,
    port: u16,
    target: String,
}

fn parse_url(url: &str) -> Result<Url, String> {
    let (tls, rest) = if let Some(rest) = url.strip_prefix("http://") {
        (false, rest)
    } else if let Some(rest) = url.strip_prefix("https://") {
        (true, rest)
    } else {
        return Err(format!("`{url}` is not an http:// or https:// URL"));
    };
    let (authority, target) = match rest.find(['/', '?']) {
        Some(i) if rest.as_bytes()[i] == b'/' => (&rest[..i], rest[i..].to_string()),
        Some(i) => (&rest[..i], format!("/{}", &rest[i..])),
        None => (rest, "/".to_string()),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !host.ends_with(']') || host.starts_with('[') => {
            (host.to_string(), port.parse::<u16>().map_err(|_| format!("bad port in `{url}`"))?)
        }
        _ => (authority.to_string(), if tls { 443 } else { 80 }),
    };
    if host.is_empty() {
        return Err(format!("no host in `{url}`"));
    }
    Ok(Url { tls, host: host.trim_start_matches('[').trim_end_matches(']').to_string(), port, target })
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Read more of the response into `buffer`; false at the end of the stream.
fn fill(socket: &Socket, buffer: &mut Vec<u8>, deadline: Instant) -> Result<bool, String> {
    let mut chunk = [0u8; 8192];
    match socket.read(&mut chunk, Some(deadline), false) {
        Ok(0) => Ok(false),
        Ok(n) => {
            if buffer.len() + n > MAX_RESPONSE {
                return Err("the response is too large".to_string());
            }
            buffer.extend_from_slice(&chunk[..n]);
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Err("timed out waiting for the response".to_string()),
        Err(e) => Err(e.to_string()),
    }
}

/// Connect to the first of the host's addresses that answers.
fn connect(host: &str, port: u16, deadline: Instant) -> Result<Socket, String> {
    let addresses = (host, port).to_socket_addrs().map_err(|e| format!("cannot resolve `{host}`: {e}"))?;
    let mut failure = format!("`{host}` has no address");
    for address in addresses {
        match Socket::connect(&address, deadline) {
            Ok(socket) => return Ok(socket),
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                return Err(format!("timed out connecting to {address}"));
            }
            Err(e) => failure = format!("cannot connect to {address}: {e}"),
        }
    }
    Err(failure)
}

fn request(method: &str, url: &str, headers: &str, body: &[u8]) -> Result<(i64, Response), String> {
    let url = parse_url(url)?;
    let deadline = Instant::now() + TIMEOUT;
    let mut socket = connect(&url.host, url.port, deadline)?;
    if url.tls {
        socket.start_tls(Session::client(&url.host, Protocol::Http)?);
    }
    let host = if url.port == if url.tls { 443 } else { 80 } {
        url.host.clone()
    } else {
        format!("{}:{}", url.host, url.port)
    };
    let mut head = format!("{method} {} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n", url.target);
    let lower = headers.to_ascii_lowercase();
    if !lower.contains("user-agent:") {
        head.push_str(&format!("User-Agent: {}\r\n", lang_names::CLI_NAME));
    }
    // Ask for gzip and decode it, unless the caller chose encodings itself (then the body is
    // left as sent), as Go's client does.
    let wants_gzip = !lower.contains("accept-encoding:");
    if wants_gzip {
        head.push_str("Accept-Encoding: gzip\r\n");
    }
    if !body.is_empty() || matches!(method, "POST" | "PUT" | "PATCH") {
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str(headers);
    head.push_str("\r\n");
    let mut out = head.into_bytes();
    out.extend_from_slice(body);
    socket.write_all(&out).map_err(|e| format!("sending the request failed: {e}"))?;

    let mut buffer = Vec::new();
    let header_end = loop {
        if let Some(at) = find(&buffer, b"\r\n\r\n") {
            break at;
        }
        if !fill(&socket, &mut buffer, deadline)? {
            return Err("the connection closed before a response".to_string());
        }
    };
    let head = std::str::from_utf8(&buffer[..header_end]).map_err(|_| "the response head is not text".to_string())?;
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status: i64 = status_line
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("not an HTTP response: `{status_line}`"))?;
    let mut header_lines: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':').map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_string())))
        .collect();
    let header = |name: &str| header_lines.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str());
    let chunked = header("transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked"));
    let length = header("content-length").and_then(|v| v.parse::<usize>().ok());
    let no_body = method == "HEAD" || status == 204 || status == 304 || (100..200).contains(&status);
    let mut rest = buffer.split_off(header_end + 4);
    let body = if no_body {
        Vec::new()
    } else if chunked {
        let mut body = Vec::new();
        loop {
            let line_end = loop {
                if let Some(at) = find(&rest, b"\r\n") {
                    break at;
                }
                if !fill(&socket, &mut rest, deadline)? {
                    return Err("the chunked body ended early".to_string());
                }
            };
            let size_text = std::str::from_utf8(&rest[..line_end]).unwrap_or("").split(';').next().unwrap_or("").trim();
            let size = usize::from_str_radix(size_text, 16).map_err(|_| "a bad chunk size".to_string())?;
            rest.drain(..line_end + 2);
            if size == 0 {
                break body;
            }
            while rest.len() < size + 2 {
                if !fill(&socket, &mut rest, deadline)? {
                    return Err("the chunked body ended early".to_string());
                }
            }
            body.extend_from_slice(&rest[..size]);
            rest.drain(..size + 2);
        }
    } else if let Some(length) = length {
        while rest.len() < length {
            if !fill(&socket, &mut rest, deadline)? {
                return Err("the connection closed in the middle of the body".to_string());
            }
        }
        rest.truncate(length);
        rest
    } else {
        while fill(&socket, &mut rest, deadline)? {}
        rest
    };
    socket.shutdown();
    let gzipped = header("content-encoding").is_some_and(|v| v.trim().eq_ignore_ascii_case("gzip"));
    let body = if wants_gzip && gzipped && !body.is_empty() {
        let body = crate::gzip::decode(&body, MAX_DECODED).map_err(|e| format!("{e} (Content-Encoding: gzip)"))?;
        header_lines.retain(|(n, _)| n != "content-encoding" && n != "content-length");
        body
    } else {
        body
    };
    let headers = header_lines.iter().map(|(n, v)| format!("{n}: {v}\n")).collect();
    Ok((status, Response { headers, body }))
}

/// Make a request. `headers` is `Name: value\r\n` lines. Returns the status, with the headers
/// and body available from the functions below, or -1 after recording the error.
///
/// # Safety
/// The pointer and length pairs describe UTF-8 strings (the body may be any bytes).
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_http_client(
    method: *const u8,
    method_len: i64,
    url: *const u8,
    url_len: i64,
    headers: *const u8,
    headers_len: i64,
    body: *const u8,
    body_len: i64,
) -> i64 {
    let method = unsafe { Str::from_raw(method, method_len) }.to_ascii_uppercase();
    let url = unsafe { Str::from_raw(url, url_len) };
    let headers = unsafe { Str::from_raw(headers, headers_len) };
    let body = unsafe { Str::from_raw(body, body_len) }.as_bytes();
    match request(&method, url, headers, body) {
        Ok((status, response)) => {
            LAST.with(|last| *last.borrow_mut() = response);
            status
        }
        Err(message) => {
            sys::record_error(message);
            -1
        }
    }
}

/// The last response's headers: lower-case `name: value` lines.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_http_client_headers() -> Str {
    LAST.with(|last| Str::from_bytes(last.borrow().headers.as_bytes()))
}

/// The last response's body, as bytes (the standard library checks whether it is text).
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_http_client_body() -> Str {
    LAST.with(|last| Str::from_bytes(&std::mem::take(&mut last.borrow_mut().body)))
}

#[cfg(test)]
mod tests {
    use super::parse_url;

    #[test]
    fn urls() {
        let url = parse_url("http://example.com:8080/a/b?c=1").unwrap();
        assert_eq!((url.host.as_str(), url.port, url.target.as_str()), ("example.com", 8080, "/a/b?c=1"));
        let url = parse_url("http://localhost?x").unwrap();
        assert_eq!((url.port, url.target.as_str()), (80, "/?x"));
        let url = parse_url("https://example.com/x").unwrap();
        assert_eq!((url.tls, url.port, url.target.as_str()), (true, 443, "/x"));
        assert!(parse_url("ftp://x").is_err());
    }
}
