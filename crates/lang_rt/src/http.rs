//! The HTTP/1.1 server underneath the `http` module (docs/05-standard-library.md, "HTTP",
//! "Implementation"), and logging.
//!
//! `lrt_http_serve` runs the accept loop on the calling thread and serves each connection on its
//! own thread, registered with the collector and counted against the task cap. For each request
//! it calls back into compiled code through a C-ABI entry function with a handle to the parsed
//! request; the compiled code reads the request through the `lrt_http_req_*` functions and leaves
//! its response with `lrt_http_respond`, which the connection then writes.
//!
//! What the parser handles: request line and headers (at most 64 KiB), `Content-Length` and
//! chunked request bodies (at most 8 MiB by default), `Expect: 100-continue`, keep-alive (the
//! HTTP/1.1 default, and HTTP/1.0 with `Connection: keep-alive`), pipelined requests, and `HEAD`
//! (no body is written). Responses get `Date`, `Server`, `Content-Length` and `Connection`
//! headers. A connection idle for 60 seconds, or slower than 30 seconds to send a request, is
//! closed. SIGINT and SIGTERM stop the accept loop; requests in progress are finished.
//!
//! The parsed request lives on the connection thread's Rust stack and holds no references to
//! collected objects. Its handle is only valid while the callback runs.

use crate::fiber;
use crate::poll::{self, IoSource, Wait};
use crate::{Str, tasks};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MAX_HEADER: usize = 64 * 1024;
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

static SHUTDOWN: AtomicBool = AtomicBool::new(false);
static ACTIVE: AtomicI64 = AtomicI64::new(0);
static MAX_BODY: AtomicI64 = AtomicI64::new(8 * 1024 * 1024);
static LISTENERS: Mutex<Vec<Option<TcpListener>>> = Mutex::new(Vec::new());

unsafe extern "C" {
    fn signal(signum: i32, handler: extern "C" fn(i32)) -> usize;
}

extern "C" fn on_signal(_: i32) {
    SHUTDOWN.store(true, Ordering::SeqCst);
}

fn set_error(message: impl std::fmt::Display) {
    // Errors go through the io module's last-error mechanism.
    crate::sys::record_error(message);
}

/// Bind a listening socket to `host:port` (an empty host means all interfaces). Returns a
/// handle, or -1 after recording the error.
///
/// # Safety
/// `ptr` and `len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_http_listen(ptr: *const u8, len: i64) -> i64 {
    let address = unsafe { Str::from_raw(ptr, len) };
    let address = if address.starts_with(':') { format!("0.0.0.0{address}") } else { address.to_string() };
    match TcpListener::bind(&address) {
        Ok(listener) => {
            let mut listeners = LISTENERS.lock().unwrap_or_else(|p| p.into_inner());
            listeners.push(Some(listener));
            listeners.len() as i64 - 1
        }
        Err(error) => {
            set_error(format!("cannot listen on {address}: {error}"));
            -1
        }
    }
}

/// The port a listener is bound to, for an address such as `:0` that asks for any free port.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_http_port(listener: i64) -> i64 {
    let listeners = LISTENERS.lock().unwrap_or_else(|p| p.into_inner());
    listeners
        .get(listener as usize)
        .and_then(Option::as_ref)
        .and_then(|l| l.local_addr().ok())
        .map_or(-1, |a| i64::from(a.port()))
}

/// The largest request body to accept, in bytes.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_http_set_max_body(bytes: i64) {
    MAX_BODY.store(bytes.max(0), Ordering::Relaxed);
}

/// Ask a running `lrt_http_serve` to stop, as SIGTERM does.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_http_shutdown() {
    SHUTDOWN.store(true, Ordering::SeqCst);
}

type Entry = extern "C-unwind" fn(*mut u8, *mut u8, i64);

struct Callback {
    entry: Entry,
    roots: usize,
}

#[repr(C)]
struct Pollfd {
    fd: i32,
    events: i16,
    revents: i16,
}

unsafe extern "C" {
    fn poll(fds: *mut Pollfd, count: u64, timeout: i32) -> i32;
}

/// Serve connections on `listener` until SIGINT, SIGTERM or `lrt_http_shutdown`, calling
/// `entry(code, env, request)` for each request. Then wait (up to ten seconds) for requests in
/// progress to finish, and return.
///
/// # Safety
/// `entry` must be the C-ABI function generated for this purpose, and `code` and `env` the
/// function value it calls.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_http_serve(listener: i64, entry: Entry, code: *mut u8, env: *mut u8) {
    use std::os::fd::AsRawFd;
    fiber::start_workers();
    raise_file_limit();
    SHUTDOWN.store(false, Ordering::SeqCst);
    // SAFETY: installing a handler that only stores to an atomic.
    unsafe {
        signal(2, on_signal);
        signal(15, on_signal);
    }
    let listener = {
        let mut listeners = LISTENERS.lock().unwrap_or_else(|p| p.into_inner());
        match listeners.get_mut(listener as usize).and_then(Option::take) {
            Some(l) => l,
            None => return,
        }
    };
    // The function value stays reachable for the collector while the server runs.
    let roots = tasks::roots(code, env);
    let callback = Callback { entry, roots: roots as usize };
    let fd = listener.as_raw_fd();
    while !SHUTDOWN.load(Ordering::SeqCst) {
        let mut pfd = Pollfd { fd, events: 1, revents: 0 };
        // SAFETY: one valid pollfd; wake every 200 ms to notice a shutdown.
        let ready = unsafe { poll(&mut pfd, 1, 200) };
        if ready <= 0 {
            continue;
        }
        // Take every connection that is waiting, each served by its own fiber.
        while let Ok((stream, peer)) = listener.accept() {
            ACTIVE.fetch_add(1, Ordering::SeqCst);
            let (entry, roots) = (callback.entry, callback.roots);
            fiber::spawn(
                [std::ptr::null_mut(); 2],
                Box::new(move |_| {
                    serve_connection(stream, peer.to_string(), entry, roots as *mut *mut u8);
                    ACTIVE.fetch_sub(1, Ordering::SeqCst);
                }),
            );
            if listener.set_nonblocking(true).is_err() {
                break;
            }
        }
        let _ = listener.set_nonblocking(false);
    }
    drop(listener);
    // Wake connections waiting for their next request, so they see the shutdown and close;
    // again every so often for ones that were busy the first time.
    let deadline = Instant::now() + Duration::from_secs(10);
    while ACTIVE.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
        poll::wake_all_io();
        std::thread::sleep(Duration::from_millis(20));
    }
    tasks::free_roots(roots);
}

/// Let the process have as many open files as the hard limit allows, as Go does: each
/// connection is one.
fn raise_file_limit() {
    #[repr(C)]
    struct Rlimit {
        current: u64,
        max: u64,
    }
    unsafe extern "C" {
        fn getrlimit(resource: i32, limit: *mut Rlimit) -> i32;
        fn setrlimit(resource: i32, limit: *const Rlimit) -> i32;
    }
    const RLIMIT_NOFILE: i32 = 7;
    let mut limit = Rlimit { current: 0, max: 0 };
    // SAFETY: plain system calls on a local structure.
    unsafe {
        if getrlimit(RLIMIT_NOFILE, &mut limit) == 0 && limit.current < limit.max {
            limit.current = limit.max;
            setrlimit(RLIMIT_NOFILE, &limit);
        }
    }
}

/// A connected socket: non-blocking, and registered with the poller when fibers are running,
/// so that waiting for it parks the task instead of blocking a thread.
pub(crate) struct Socket {
    stream: TcpStream,
    source: Option<&'static IoSource>,
}

impl Socket {
    pub(crate) fn new(stream: TcpStream) -> Socket {
        use std::os::fd::AsRawFd;
        let _ = stream.set_nodelay(true);
        let _ = stream.set_nonblocking(true);
        let source = IoSource::register(stream.as_raw_fd());
        Socket { stream, source }
    }

    fn wait(&self, write: bool, deadline: Option<Instant>) -> Wait {
        use std::os::fd::AsRawFd;
        match self.source {
            Some(source) => source.wait(write, deadline),
            None => poll::wait_blocking(self.stream.as_raw_fd(), write, deadline),
        }
    }

    /// Read some bytes, waiting until `deadline` for them. `Ok(0)` at the end of the stream;
    /// an error of kind `TimedOut` at the deadline or, with `stop_at_shutdown`, at shutdown.
    pub(crate) fn read(
        &self,
        buffer: &mut [u8],
        deadline: Option<Instant>,
        stop_at_shutdown: bool,
    ) -> std::io::Result<usize> {
        use std::io::ErrorKind;
        loop {
            if stop_at_shutdown && SHUTDOWN.load(Ordering::SeqCst) {
                return Err(ErrorKind::TimedOut.into());
            }
            match (&self.stream).read(buffer) {
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    if self.wait(false, deadline) == Wait::TimedOut {
                        return Err(ErrorKind::TimedOut.into());
                    }
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                other => return other,
            }
        }
    }

    /// Write all of `bytes`, waiting while the peer's window is full (for at most a minute).
    pub(crate) fn write_all(&self, mut bytes: &[u8]) -> std::io::Result<()> {
        use std::io::ErrorKind;
        let deadline = Instant::now() + IDLE_TIMEOUT;
        while !bytes.is_empty() {
            match (&self.stream).write(bytes) {
                Ok(0) => return Err(ErrorKind::WriteZero.into()),
                Ok(n) => bytes = &bytes[n..],
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    if self.wait(true, Some(deadline)) == Wait::TimedOut {
                        return Err(ErrorKind::TimedOut.into());
                    }
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    pub(crate) fn shutdown(&self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        if let Some(source) = self.source.take() {
            source.deregister();
        }
    }
}

pub struct Request {
    method: String,
    /// The path without the query string, percent-decoded.
    path: String,
    query: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    remote: String,
    head: bool,
    /// Status, header lines, and body.
    response: Option<(i64, String, Vec<u8>)>,
    /// The connection the request came on, while the handler runs.
    connection: *mut Connection,
    /// The WebSocket id, once the handler has upgraded the connection.
    upgraded: Option<i64>,
}

impl Request {
    pub(crate) fn method(&self) -> &str {
        &self.method
    }

    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    pub(crate) fn remote(&self) -> &str {
        &self.remote
    }

    pub(crate) fn headers(&self) -> &[(String, String)] {
        &self.headers
    }

    /// A header's value; `name` is lower-case.
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    /// For a WebSocket upgrade: the connection's socket and the bytes already read past the
    /// request.
    pub(crate) fn take_connection(&mut self) -> Option<(Arc<Socket>, Vec<u8>)> {
        // SAFETY: `connection` points at the connection serving this request, which outlives
        // the handler call.
        let connection = unsafe { self.connection.as_mut()? };
        Some((connection.socket.clone(), std::mem::take(&mut connection.buffer)))
    }

    pub(crate) fn set_upgraded(&mut self, id: i64) {
        self.upgraded = Some(id);
    }
}

pub(crate) fn shutting_down() -> bool {
    SHUTDOWN.load(Ordering::SeqCst)
}

/// Why a request could not be read.
enum ReadError {
    /// The connection closed or timed out between requests: close quietly.
    Closed,
    /// Answer with this status and close.
    Status(u16, &'static str),
}

struct Connection {
    socket: Arc<Socket>,
    buffer: Vec<u8>,
}

impl Connection {
    /// Read more bytes into the buffer. With `idle`, waits for the next request, noticing a
    /// shutdown; otherwise the request must arrive within the request timeout.
    fn fill(&mut self, idle: bool, started: Instant) -> Result<(), ReadError> {
        let deadline = started + if idle { IDLE_TIMEOUT } else { REQUEST_TIMEOUT };
        // Read straight into the buffer's spare room, which starts small: an idle connection
        // should cost little memory.
        if self.buffer.capacity() - self.buffer.len() < 1024 {
            self.buffer.reserve(self.buffer.len().max(4096));
        }
        let spare = self.buffer.spare_capacity_mut();
        // SAFETY: the bytes are only read after `read` has written them, and `set_len` covers
        // only those.
        let room = unsafe { std::slice::from_raw_parts_mut(spare.as_mut_ptr() as *mut u8, spare.len()) };
        match self.socket.read(room, Some(deadline), idle) {
            Ok(0) => Err(ReadError::Closed),
            Ok(n) => {
                unsafe { self.buffer.set_len(self.buffer.len() + n) };
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut && !idle => {
                Err(ReadError::Status(408, "Request Timeout"))
            }
            Err(_) => Err(ReadError::Closed),
        }
    }

    /// The next request, or why there is none. Whether to keep the connection open is returned
    /// with it.
    fn read_request(&mut self, remote: &str) -> Result<(Request, bool), ReadError> {
        let started = Instant::now();
        let header_end = loop {
            if let Some(at) = find(&self.buffer, b"\r\n\r\n") {
                break at;
            }
            if self.buffer.len() > MAX_HEADER {
                return Err(ReadError::Status(431, "Request Header Fields Too Large"));
            }
            let idle = self.buffer.is_empty();
            self.fill(idle, started)?;
        };
        let head =
            std::str::from_utf8(&self.buffer[..header_end]).map_err(|_| ReadError::Status(400, "Bad Request"))?;
        let mut lines = head.split("\r\n");
        let request_line = lines.next().unwrap_or_default();
        let mut parts = request_line.split(' ');
        let (Some(method), Some(target), Some(version), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(ReadError::Status(400, "Bad Request"));
        };
        if method.is_empty() || !method.bytes().all(|b| b.is_ascii_uppercase()) || !target.starts_with('/') {
            return Err(ReadError::Status(400, "Bad Request"));
        }
        let http10 = match version {
            "HTTP/1.1" => false,
            "HTTP/1.0" => true,
            _ => return Err(ReadError::Status(505, "HTTP Version Not Supported")),
        };
        let mut headers = Vec::new();
        for line in lines {
            let Some((name, value)) = line.split_once(':') else {
                return Err(ReadError::Status(400, "Bad Request"));
            };
            if name.is_empty() || name.ends_with(' ') || name.ends_with('\t') {
                return Err(ReadError::Status(400, "Bad Request"));
            }
            headers.push((name.to_ascii_lowercase(), value.trim().to_string()));
        }
        let header = |name: &str| headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str());
        let connection = header("connection").map(str::to_ascii_lowercase);
        let keep_alive = match connection.as_deref() {
            Some(c) if c.contains("close") => false,
            Some(c) if c.contains("keep-alive") => true,
            _ => !http10,
        };
        let chunked = header("transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked"));
        let length = match header("content-length") {
            Some(text) if !chunked => Some(text.parse::<usize>().map_err(|_| ReadError::Status(400, "Bad Request"))?),
            _ => None,
        };
        let max_body = MAX_BODY.load(Ordering::Relaxed) as usize;
        if length.is_some_and(|n| n > max_body) {
            return Err(ReadError::Status(413, "Content Too Large"));
        }
        let expects_continue = header("expect").is_some_and(|v| v.eq_ignore_ascii_case("100-continue"));
        let (method, target) = (method.to_string(), target.to_string());
        self.buffer.drain(..header_end + 4);
        if expects_continue && (chunked || length.is_some_and(|n| n > 0)) {
            let _ = self.socket.write_all(b"HTTP/1.1 100 Continue\r\n\r\n");
        }
        let body = if chunked {
            self.read_chunked(max_body, started)?
        } else {
            let length = length.unwrap_or(0);
            while self.buffer.len() < length {
                self.fill(false, started)?;
            }
            self.buffer.drain(..length).collect()
        };
        let (raw_path, query) = target.split_once('?').unwrap_or((&target, ""));
        let path = percent_decode(raw_path, false).unwrap_or_else(|| raw_path.to_string());
        let request = Request {
            head: method == "HEAD",
            method,
            path,
            query: query.to_string(),
            headers,
            body,
            remote: remote.to_string(),
            response: None,
            connection: std::ptr::null_mut(),
            upgraded: None,
        };
        Ok((request, keep_alive))
    }

    fn read_chunked(&mut self, max_body: usize, started: Instant) -> Result<Vec<u8>, ReadError> {
        let mut body = Vec::new();
        loop {
            let line_end = loop {
                if let Some(at) = find(&self.buffer, b"\r\n") {
                    break at;
                }
                self.fill(false, started)?;
            };
            let size_text =
                std::str::from_utf8(&self.buffer[..line_end]).map_err(|_| ReadError::Status(400, "Bad Request"))?;
            let size_text = size_text.split(';').next().unwrap_or_default().trim();
            let size = usize::from_str_radix(size_text, 16).map_err(|_| ReadError::Status(400, "Bad Request"))?;
            self.buffer.drain(..line_end + 2);
            if size == 0 {
                // Trailers, ending with an empty line.
                loop {
                    let end = loop {
                        if let Some(at) = find(&self.buffer, b"\r\n") {
                            break at;
                        }
                        self.fill(false, started)?;
                    };
                    self.buffer.drain(..end + 2);
                    if end == 0 {
                        return Ok(body);
                    }
                }
            }
            if body.len() + size > max_body {
                return Err(ReadError::Status(413, "Content Too Large"));
            }
            while self.buffer.len() < size + 2 {
                self.fill(false, started)?;
            }
            body.extend_from_slice(&self.buffer[..size]);
            self.buffer.drain(..size + 2);
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn hex(b: u8) -> Option<u8> {
    (b as char).to_digit(16).map(|d| d as u8)
}

/// Decode `%XX` escapes (and, in query strings, `+` as a space). `None` if the result is not
/// valid UTF-8 or an escape is malformed.
fn percent_decode(text: &str, plus_is_space: bool) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let high = hex(*bytes.get(i + 1)?)?;
                let low = hex(*bytes.get(i + 2)?)?;
                out.push(high * 16 + low);
                i += 3;
            }
            b'+' if plus_is_space => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

fn reason(status: i64) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        304 => "Not Modified",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        413 => "Content Too Large",
        415 => "Unsupported Media Type",
        422 => "Unprocessable Content",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        505 => "HTTP Version Not Supported",
        _ => "",
    }
}

/// An HTTP date, `Wed, 18 Nov 2026 12:00:00 GMT`, cached for the current second.
fn http_date() -> String {
    static CACHE: Mutex<(u64, String)> = Mutex::new((0, String::new()));
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let mut cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
    if cache.0 != now || cache.1.is_empty() {
        cache.0 = now;
        cache.1 = format_http_date(now);
    }
    cache.1.clone()
}

/// Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn format_http_date(seconds: u64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let days = (seconds / 86_400) as i64;
    let (year, month, day) = civil(days);
    let rest = seconds % 86_400;
    format!(
        "{}, {:02} {} {} {:02}:{:02}:{:02} GMT",
        DAYS[(days % 7) as usize],
        day,
        MONTHS[(month - 1) as usize],
        year,
        rest / 3600,
        rest / 60 % 60,
        rest % 60
    )
}

fn write_simple(socket: &Socket, status: u16, text: &str) {
    let response = format!(
        "HTTP/1.1 {status} {text}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}\n",
        text.len() + 1
    );
    let _ = socket.write_all(response.as_bytes());
}

fn serve_connection(stream: TcpStream, remote: String, entry: Entry, roots: *mut *mut u8) {
    let mut connection = Connection { socket: Arc::new(Socket::new(stream)), buffer: Vec::new() };
    loop {
        let (mut request, keep_alive) = match connection.read_request(&remote) {
            Ok(request) => request,
            Err(ReadError::Closed) => break,
            Err(ReadError::Status(status, text)) => {
                write_simple(&connection.socket, status, text);
                break;
            }
        };
        // SAFETY: `roots` holds the function value for the server's lifetime.
        let (code, env) = unsafe { (roots.read(), roots.add(1).read()) };
        request.connection = &mut connection;
        let handle = &mut request as *mut Request as i64;
        // A panicking handler ends only this request, with a 500; the panic was reported
        // where it happened. Mutexes it held are released.
        let handled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| entry(code, env, handle)));
        request.connection = std::ptr::null_mut();
        let panicked = handled.is_err();
        if panicked {
            if let Some(me) = fiber::current() {
                fiber::release_held(me);
            }
            request.response = None;
        }
        if let Some(id) = request.upgraded {
            // The handler has run the WebSocket to its end; the connection is finished.
            crate::ws::forget(id);
            break;
        }
        let keep_alive = keep_alive && !panicked && !SHUTDOWN.load(Ordering::SeqCst);
        let (status, header_lines, body) = request.response.take().unwrap_or_else(|| {
            (500, "Content-Type: text/plain; charset=utf-8\r\n".to_string(), b"internal server error\n".to_vec())
        });
        let mut out = Vec::with_capacity(256 + body.len());
        out.extend_from_slice(format!("HTTP/1.1 {status} {}\r\n", reason(status)).as_bytes());
        out.extend_from_slice(header_lines.as_bytes());
        out.extend_from_slice(format!("Date: {}\r\n", http_date()).as_bytes());
        out.extend_from_slice(format!("Server: {}\r\n", lang_names::CLI_NAME).as_bytes());
        if status != 204 && status != 304 {
            out.extend_from_slice(format!("Content-Length: {}\r\n", body.len()).as_bytes());
        }
        out.extend_from_slice(if keep_alive {
            b"Connection: keep-alive\r\n\r\n"
        } else {
            b"Connection: close\r\n\r\n"
        });
        if !request.head && status != 204 && status != 304 {
            out.extend_from_slice(&body);
        }
        if connection.socket.write_all(&out).is_err() || !keep_alive {
            break;
        }
        if connection.buffer.is_empty() && connection.buffer.capacity() > 64 * 1024 {
            connection.buffer = Vec::new();
        }
    }
    connection.socket.shutdown();
}

// ---- the request, as seen from compiled code --------------------------------------------

/// # Safety
/// `handle` is a request passed to the entry function, which is still running.
pub(crate) unsafe fn request<'a>(handle: i64) -> &'a mut Request {
    unsafe { &mut *(handle as *mut Request) }
}

pub(crate) fn optional(value: Option<&str>) -> Str {
    match value {
        Some(text) => Str::from_bytes(text.as_bytes()),
        None => Str { ptr: std::ptr::null(), len: 0 },
    }
}

/// # Safety
/// `handle` is a live request.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_http_req_method(handle: i64) -> Str {
    Str::from_bytes(unsafe { request(handle) }.method.as_bytes())
}

/// # Safety
/// `handle` is a live request.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_http_req_path(handle: i64) -> Str {
    Str::from_bytes(unsafe { request(handle) }.path.as_bytes())
}

/// # Safety
/// `handle` is a live request.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_http_req_remote(handle: i64) -> Str {
    Str::from_bytes(unsafe { request(handle) }.remote.as_bytes())
}

/// The first query parameter with this name, decoded, or `None`.
///
/// # Safety
/// `handle` is a live request; `ptr` and `len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_http_req_query(handle: i64, ptr: *const u8, len: i64) -> Str {
    let wanted = unsafe { Str::from_raw(ptr, len) };
    optional(query_value(&unsafe { request(handle) }.query, wanted).as_deref())
}

/// The first parameter called `wanted` in a query string, decoded.
pub(crate) fn query_value(query: &str, wanted: &str) -> Option<String> {
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        if percent_decode(name, true).as_deref() == Some(wanted) {
            return Some(percent_decode(value, true).unwrap_or_else(|| value.to_string()));
        }
    }
    None
}

/// A header's value (names are case-insensitive), or `None`.
///
/// # Safety
/// `handle` is a live request; `ptr` and `len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_http_req_header(handle: i64, ptr: *const u8, len: i64) -> Str {
    let wanted = unsafe { Str::from_raw(ptr, len) }.to_ascii_lowercase();
    let request = unsafe { request(handle) };
    optional(request.headers.iter().find(|(n, _)| *n == wanted).map(|(_, v)| v.as_str()))
}

/// The body as text; `None` if it is not valid UTF-8.
///
/// # Safety
/// `handle` is a live request.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_http_req_body(handle: i64) -> Str {
    optional(std::str::from_utf8(&unsafe { request(handle) }.body).ok())
}

/// Leave the response: its status, header lines (`Name: value\r\n` each) and body.
///
/// # Safety
/// `handle` is a live request; the strings are UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_http_respond(
    handle: i64,
    status: i64,
    headers: *const u8,
    headers_len: i64,
    body: *const u8,
    body_len: i64,
) {
    let headers = unsafe { Str::from_raw(headers, headers_len) }.to_string();
    let body = unsafe { Str::from_raw(body, body_len) }.as_bytes().to_vec();
    unsafe { request(handle) }.response = Some((status, headers, body));
}

// ---- logging ----------------------------------------------------------------------------

static LOG_LEVEL: AtomicI64 = AtomicI64::new(-1);

fn level_number(level: &str) -> i64 {
    match level.to_ascii_lowercase().as_str() {
        "debug" => 0,
        "info" => 1,
        "warn" | "warning" => 2,
        "error" => 3,
        "off" | "none" => 4,
        _ => 1,
    }
}

fn log_level() -> i64 {
    let level = LOG_LEVEL.load(Ordering::Relaxed);
    if level >= 0 {
        return level;
    }
    let level = std::env::var(lang_names::env_var("LOG")).map(|v| level_number(&v)).unwrap_or(1);
    LOG_LEVEL.store(level, Ordering::Relaxed);
    level
}

/// Set the lowest level that is written: `debug`, `info`, `warn`, `error` or `off`.
///
/// # Safety
/// `ptr` and `len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_log_set_level(ptr: *const u8, len: i64) {
    LOG_LEVEL.store(level_number(unsafe { Str::from_raw(ptr, len) }), Ordering::Relaxed);
}

/// Write `2026-11-18T12:00:00.123Z INFO message` to standard error, if the level is enabled.
///
/// # Safety
/// The arguments describe UTF-8 strings.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_log_write(level: *const u8, level_len: i64, message: *const u8, message_len: i64) {
    let level = unsafe { Str::from_raw(level, level_len) };
    if level_number(level) < log_level() {
        return;
    }
    let message = unsafe { Str::from_raw(message, message_len) };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let seconds = now.as_secs();
    let (year, month, day) = civil((seconds / 86_400) as i64);
    let rest = seconds % 86_400;
    let line = format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z {:<5} {message}\n",
        rest / 3600,
        rest / 60 % 60,
        rest % 60,
        now.subsec_millis(),
        level.to_ascii_uppercase()
    );
    crate::out::flush();
    let _ = std::io::stderr().lock().write_all(line.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_formatted_as_http_wants() {
        assert_eq!(format_http_date(0), "Thu, 01 Jan 1970 00:00:00 GMT");
        assert_eq!(format_http_date(1_795_000_000), "Wed, 18 Nov 2026 11:06:40 GMT");
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("a%20b+c", true).as_deref(), Some("a b c"));
        assert_eq!(percent_decode("a+b", false).as_deref(), Some("a+b"));
        assert_eq!(percent_decode("%zz", true), None);
        assert_eq!(percent_decode("%C3%A9", false).as_deref(), Some("é"));
    }
}
