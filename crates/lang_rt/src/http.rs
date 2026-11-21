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
//! The parsed request lives on the connection task's stack, as offsets into the connection's
//! buffer, and holds no references to collected objects. Its handle is only valid while the
//! callback runs. Responses are written into a per-connection output buffer, which is sent when
//! the connection is about to wait for more input (docs/10, milestone 2).

use crate::fiber;
use crate::metrics;
use crate::poll::{self, IoSource, Wait};
use crate::tls::{self, Session};
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
/// SIGTERM or `lrt_http_shutdown` asked the server to stop: it stops after the drain delay.
static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);
/// Seconds to keep serving after a stop is asked for, with readiness failing, so that a load
/// balancer takes the instance out before it closes (`server.drain_delay`).
static DRAIN_SECONDS: AtomicI64 = AtomicI64::new(0);
/// A stop has been asked for and the drain delay is running.
static DRAINING: AtomicBool = AtomicBool::new(false);
static ACTIVE: AtomicI64 = AtomicI64::new(0);
static MAX_BODY: AtomicI64 = AtomicI64::new(8 * 1024 * 1024);
static LISTENERS: Mutex<Vec<Option<Listener>>> = Mutex::new(Vec::new());

/// A listening socket, and the TLS configuration for its connections if it serves HTTPS.
struct Listener {
    socket: TcpListener,
    tls: Option<Arc<rustls::ServerConfig>>,
}

unsafe extern "C" {
    fn signal(signum: i32, handler: extern "C" fn(i32)) -> usize;
}

extern "C" fn on_signal(signum: i32) {
    // SIGINT (someone at a terminal) stops at once; SIGTERM (a supervisor) drains first.
    if signum == 15 {
        STOP_REQUESTED.store(true, Ordering::SeqCst);
    } else {
        SHUTDOWN.store(true, Ordering::SeqCst);
    }
}

/// Open connections, WebSockets included.
pub(crate) fn open_connections() -> i64 {
    ACTIVE.load(Ordering::Relaxed).max(0)
}

/// How long to go on serving after SIGTERM, with readiness failing.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_http_set_drain(seconds: i64) {
    DRAIN_SECONDS.store(seconds.max(0), Ordering::Relaxed);
}

/// Whether the server is draining before it stops: 1 or 0.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_http_draining() -> i64 {
    i64::from(DRAINING.load(Ordering::Relaxed))
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
    listen(unsafe { Str::from_raw(ptr, len) }, None)
}

/// `lrt_http_listen` for HTTPS, with a PEM certificate chain and private key.
///
/// # Safety
/// The pointer and length pairs describe UTF-8 strings.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_http_listen_tls(
    ptr: *const u8,
    len: i64,
    cert: *const u8,
    cert_len: i64,
    key: *const u8,
    key_len: i64,
) -> i64 {
    let (cert, key) = unsafe { (Str::from_raw(cert, cert_len), Str::from_raw(key, key_len)) };
    match tls::server_config(cert, key) {
        Ok(config) => listen(unsafe { Str::from_raw(ptr, len) }, Some(config)),
        Err(message) => {
            set_error(message);
            -1
        }
    }
}

fn listen(address: &str, tls: Option<Arc<rustls::ServerConfig>>) -> i64 {
    let address = if address.starts_with(':') { format!("0.0.0.0{address}") } else { address.to_string() };
    match TcpListener::bind(&address) {
        Ok(socket) => {
            let mut listeners = LISTENERS.lock().unwrap_or_else(|p| p.into_inner());
            listeners.push(Some(Listener { socket, tls }));
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
        .and_then(|l| l.socket.local_addr().ok())
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
    STOP_REQUESTED.store(true, Ordering::SeqCst);
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
/// `entry(code, env, request)` for each request. After SIGTERM or `lrt_http_shutdown`, go on
/// serving for the drain delay first. Then wait (up to ten seconds) for requests in progress to
/// finish, and return.
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
    STOP_REQUESTED.store(false, Ordering::SeqCst);
    DRAINING.store(false, Ordering::SeqCst);
    let mut drain_until: Option<Instant> = None;
    // SAFETY: installing a handler that only stores to an atomic.
    unsafe {
        signal(2, on_signal);
        signal(15, on_signal);
    }
    let Listener { socket: listener, tls } = {
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
    crate::log::start_buffering();
    while !SHUTDOWN.load(Ordering::SeqCst) {
        // Log lines wait here at most until the loop next wakes.
        crate::log::flush();
        if STOP_REQUESTED.load(Ordering::SeqCst) {
            let until = *drain_until.get_or_insert_with(|| {
                let seconds = DRAIN_SECONDS.load(Ordering::Relaxed);
                if seconds > 0 {
                    DRAINING.store(true, Ordering::SeqCst);
                    crate::log::write_line("info", &format!("draining for {seconds} s before stopping"), "");
                }
                Instant::now() + Duration::from_secs(seconds as u64)
            });
            if Instant::now() >= until {
                SHUTDOWN.store(true, Ordering::SeqCst);
                break;
            }
        }
        let mut pfd = Pollfd { fd, events: 1, revents: 0 };
        // SAFETY: one valid pollfd; wake every 200 ms to notice a shutdown.
        let ready = unsafe { poll(&mut pfd, 1, 200) };
        if ready <= 0 {
            continue;
        }
        // Take every connection that is waiting, each served by its own fiber.
        while let Ok((stream, peer)) = listener.accept() {
            ACTIVE.fetch_add(1, Ordering::SeqCst);
            if metrics::on() {
                metrics::connection_opened();
            }
            let (entry, roots) = (callback.entry, callback.roots);
            let tls = tls.clone();
            fiber::spawn(
                [std::ptr::null_mut(); 2],
                fiber::Entry::Closure(Box::new(move |_| {
                    serve_connection(stream, tls, peer, entry, roots as *mut *mut u8);
                    ACTIVE.fetch_sub(1, Ordering::SeqCst);
                })),
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
    crate::log::stop_buffering();
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
///
/// With a TLS session, reads and writes go through it; the socket carries the encrypted records.
pub(crate) struct Socket {
    stream: TcpStream,
    source: Option<&'static IoSource>,
    tls: Option<Session>,
    /// The last read returned less than it asked for, so the socket was empty then; the next
    /// read waits for an edge first instead of making a `recv` that would find nothing. With
    /// edge-triggered polling, bytes that arrive after a read always produce a new edge.
    drained: AtomicBool,
}

/// When a wait on a socket gives up.
#[derive(Clone, Copy)]
pub(crate) enum Deadline {
    Never,
    /// An `Instant`, converted only if the task actually waits.
    At(Instant),
    /// This long from when the task starts to wait; the clock is read only then.
    After(Duration),
    /// On the poller's coarse clock (`poll::deadline_after`).
    Coarse(u64),
}

impl Deadline {
    fn coarse(self) -> Option<u64> {
        match self {
            Deadline::Never => None,
            Deadline::At(at) => Some(poll::deadline_at(at)),
            Deadline::After(after) => Some(poll::deadline_after(after)),
            Deadline::Coarse(at) => Some(at),
        }
    }
}

impl From<Option<Instant>> for Deadline {
    fn from(deadline: Option<Instant>) -> Deadline {
        deadline.map_or(Deadline::Never, Deadline::At)
    }
}

impl Socket {
    pub(crate) fn with_tls(stream: TcpStream, tls: Option<Session>) -> Socket {
        use std::os::fd::AsRawFd;
        let _ = stream.set_nodelay(true);
        let _ = stream.set_nonblocking(true);
        let source = IoSource::register(stream.as_raw_fd());
        Socket { stream, source, tls, drained: AtomicBool::new(false) }
    }

    /// Connect to `address` without blocking the worker thread: the task parks until the
    /// connection is made, refused, or the deadline passes.
    pub(crate) fn connect(address: &std::net::SocketAddr, deadline: Instant) -> std::io::Result<Socket> {
        use std::net::SocketAddr;
        use std::os::fd::FromRawFd;
        unsafe extern "C" {
            fn socket(domain: i32, kind: i32, protocol: i32) -> i32;
            fn connect(fd: i32, address: *const u8, length: u32) -> i32;
        }
        const SOCK_STREAM: i32 = 1;
        const SOCK_NONBLOCK: i32 = 0o4000;
        const SOCK_CLOEXEC: i32 = 0o2000000;
        const EINPROGRESS: i32 = 115;
        const EINTR: i32 = 4;
        // `struct sockaddr_in` and `struct sockaddr_in6`, built by hand.
        let mut raw = [0u8; 28];
        let (domain, length) = match address {
            SocketAddr::V4(v4) => {
                raw[0..2].copy_from_slice(&2u16.to_ne_bytes());
                raw[2..4].copy_from_slice(&v4.port().to_be_bytes());
                raw[4..8].copy_from_slice(&v4.ip().octets());
                (2, 16)
            }
            SocketAddr::V6(v6) => {
                raw[0..2].copy_from_slice(&10u16.to_ne_bytes());
                raw[2..4].copy_from_slice(&v6.port().to_be_bytes());
                raw[4..8].copy_from_slice(&v6.flowinfo().to_be_bytes());
                raw[8..24].copy_from_slice(&v6.ip().octets());
                raw[24..28].copy_from_slice(&v6.scope_id().to_ne_bytes());
                (10, 28)
            }
        };
        // SAFETY: plain system calls; the descriptor is owned by the stream from here on.
        let fd = unsafe { socket(domain, SOCK_STREAM | SOCK_NONBLOCK | SOCK_CLOEXEC, 0) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let stream = unsafe { TcpStream::from_raw_fd(fd) };
        let started = unsafe { connect(fd, raw.as_ptr(), length) };
        let in_progress = started != 0 && {
            let error = std::io::Error::last_os_error();
            match error.raw_os_error() {
                Some(EINPROGRESS | EINTR) => true,
                _ => return Err(error),
            }
        };
        let socket = Socket::with_tls(stream, None);
        if in_progress {
            loop {
                if socket.wait(true, Deadline::At(deadline)) == Wait::TimedOut {
                    return Err(std::io::ErrorKind::TimedOut.into());
                }
                if let Some(error) = socket.stream.take_error()? {
                    return Err(error);
                }
                if socket.stream.peer_addr().is_ok() {
                    break;
                }
            }
        }
        Ok(socket)
    }

    /// Run the connection through a TLS session from now on.
    pub(crate) fn start_tls(&mut self, session: Session) {
        self.tls = Some(session);
    }

    fn wait(&self, write: bool, deadline: Deadline) -> Wait {
        use std::os::fd::AsRawFd;
        match self.source {
            Some(source) => source.wait(write, deadline.coarse()),
            None => poll::wait_blocking(self.stream.as_raw_fd(), write, deadline.coarse()),
        }
    }

    /// Read some bytes, waiting until `deadline` for them. `Ok(0)` at the end of the stream;
    /// an error of kind `TimedOut` at the deadline or, with `stop_at_shutdown`, at shutdown.
    pub(crate) fn read(
        &self,
        buffer: &mut [u8],
        deadline: impl Into<Deadline>,
        stop_at_shutdown: bool,
    ) -> std::io::Result<usize> {
        use std::io::ErrorKind;
        let deadline = deadline.into();
        if let Some(session) = &self.tls {
            return self.read_tls(session, buffer, deadline, stop_at_shutdown);
        }
        // Only a registered socket gets edges; a blocking-mode wait polls the descriptor.
        if self.source.is_some() && self.drained.swap(false, Ordering::Relaxed) {
            if stop_at_shutdown && SHUTDOWN.load(Ordering::SeqCst) {
                return Err(ErrorKind::TimedOut.into());
            }
            if self.wait(false, deadline) == Wait::TimedOut {
                return Err(ErrorKind::TimedOut.into());
            }
        }
        loop {
            if stop_at_shutdown && SHUTDOWN.load(Ordering::SeqCst) {
                return Err(ErrorKind::TimedOut.into());
            }
            match (&self.stream).read(buffer) {
                Ok(n) => {
                    // A short read emptied the socket. Not after 0 bytes (the end of the
                    // stream) or a full buffer (more may be waiting).
                    if n > 0 && n < buffer.len() {
                        self.drained.store(true, Ordering::Relaxed);
                    }
                    return Ok(n);
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    if self.wait(false, deadline) == Wait::TimedOut {
                        return Err(ErrorKind::TimedOut.into());
                    }
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }

    /// Write all of `bytes`, waiting while the peer's window is full (for at most a minute
    /// each time). The clock is read only if the write has to wait.
    pub(crate) fn write_all(&self, mut bytes: &[u8]) -> std::io::Result<()> {
        use std::io::ErrorKind;
        if let Some(session) = &self.tls {
            return self.write_tls(session, bytes);
        }
        while !bytes.is_empty() {
            match (&self.stream).write(bytes) {
                Ok(0) => return Err(ErrorKind::WriteZero.into()),
                Ok(n) => bytes = &bytes[n..],
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    if self.wait(true, Deadline::After(IDLE_TIMEOUT)) == Wait::TimedOut {
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
        if let Some(session) = &self.tls {
            // Say goodbye properly if the socket will take it now; never wait for it.
            let mut connection = session.lock();
            connection.send_close_notify();
            let _ = self.flush_tls(&mut connection);
        }
        let _ = self.stream.shutdown(Shutdown::Both);
    }

    /// Write out the records the TLS session has ready. `Ok(false)` if the socket is full
    /// before they are all written.
    fn flush_tls(&self, connection: &mut rustls::Connection) -> std::io::Result<bool> {
        use std::io::ErrorKind;
        while connection.wants_write() {
            match connection.write_tls(&mut &self.stream) {
                Ok(_) => {}
                Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(false),
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(true)
    }

    /// `read` through the TLS session: plaintext it already has, or else more records from the
    /// socket (which may first mean writing handshake records).
    fn read_tls(
        &self,
        session: &Session,
        buffer: &mut [u8],
        deadline: Deadline,
        stop_at_shutdown: bool,
    ) -> std::io::Result<usize> {
        use std::io::ErrorKind;
        loop {
            if stop_at_shutdown && SHUTDOWN.load(Ordering::SeqCst) {
                return Err(ErrorKind::TimedOut.into());
            }
            let wait_for_write = {
                let mut connection = session.lock();
                match connection.reader().read(buffer) {
                    Ok(n) => return Ok(n),
                    Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                    // The peer closed without a close_notify; for HTTP that is an ordinary end.
                    Err(e) if e.kind() == ErrorKind::UnexpectedEof => return Ok(0),
                    Err(e) => return Err(e),
                }
                if !self.flush_tls(&mut connection)? {
                    true
                } else {
                    match connection.read_tls(&mut &self.stream) {
                        Ok(0) => return Ok(0),
                        Ok(_) => {
                            if let Err(error) = connection.process_new_packets() {
                                // Send the alert that explains the failure, if the socket takes it.
                                let _ = self.flush_tls(&mut connection);
                                return Err(std::io::Error::new(ErrorKind::InvalidData, error));
                            }
                            continue;
                        }
                        Err(e) if e.kind() == ErrorKind::WouldBlock => false,
                        Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                        Err(e) => return Err(e),
                    }
                }
            };
            // The session's lock is released while the task waits.
            if self.wait(wait_for_write, deadline) == Wait::TimedOut {
                return Err(ErrorKind::TimedOut.into());
            }
        }
    }

    /// `write_all` through the TLS session.
    fn write_tls(&self, session: &Session, mut bytes: &[u8]) -> std::io::Result<()> {
        use std::io::ErrorKind;
        loop {
            let flushed = {
                let mut connection = session.lock();
                while !bytes.is_empty() {
                    let n = connection.writer().write(bytes)?;
                    if n == 0 {
                        break;
                    }
                    bytes = &bytes[n..];
                }
                self.flush_tls(&mut connection)?
            };
            if flushed && bytes.is_empty() {
                return Ok(());
            }
            if !flushed && self.wait(true, Deadline::After(IDLE_TIMEOUT)) == Wait::TimedOut {
                return Err(ErrorKind::TimedOut.into());
            }
        }
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        if let Some(source) = self.source.take() {
            source.deregister();
        }
    }
}

/// A range of bytes in the connection's buffer: start and end offsets.
#[derive(Clone, Copy, Default)]
struct Span {
    start: u32,
    end: u32,
}

impl Span {
    fn new(start: usize, end: usize) -> Span {
        Span { start: start as u32, end: end as u32 }
    }
}

#[derive(Clone, Copy, Default)]
struct Header {
    name: Span,
    value: Span,
}

/// Headers kept in the request itself; more go to a `Vec`.
const INLINE_HEADERS: usize = 32;

/// The methods whose names are handed to compiled code without a copy.
const METHODS: [&str; 7] = ["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS"];

enum Method {
    Known(&'static str),
    Other(Span),
}

enum Path {
    /// No `%` escapes: the bytes in the buffer.
    Raw(Span),
    /// Decoded, with the path as sent.
    Decoded(String, Span),
}

enum Body {
    InBuffer(Span),
    /// A chunked body, decoded.
    Owned(Vec<u8>),
}

/// A parsed request: offsets into its connection's buffer, which holds its head and body
/// unchanged until the response is written (docs/10, milestone 2). Nothing is copied unless the
/// handler asks for it, and then straight into collected memory.
pub struct Request {
    method: Method,
    /// The path without the query string, percent-decoded.
    path: Path,
    query: Span,
    headers: [Header; INLINE_HEADERS],
    header_count: usize,
    more_headers: Vec<Header>,
    body: Body,
    head: bool,
    /// Whether the response has been given (`lrt_http_respond`).
    responded: bool,
    /// Where the connection's output stood before this request's response.
    out_start: usize,
    /// Where this request ends in the buffer.
    end: usize,
    /// Whether the connection stays open after this request.
    keep_alive: bool,
    /// The connection the request came on. It outlives the request.
    connection: *mut Connection,
    /// The WebSocket id, once the handler has upgraded the connection.
    upgraded: Option<i64>,
    /// A file `lrt_http_static` chose, with its status, for `lrt_http_respond` to send.
    file: Option<(i64, crate::static_files::Part)>,
    /// A file too large for the output buffer, sent by `serve_connection` after the handler.
    stream: Option<crate::static_files::Part>,
    /// The metrics slot of the route that took the request (`metrics::UNMATCHED` until one
    /// does), and the route's flags ([`ROUTE_UNMETERED`]; 2 for the access log).
    route: u16,
    route_flags: u8,
    /// The status and body size of the response, for the metrics.
    status: i64,
    bytes: u64,
}

impl Request {
    fn connection(&self) -> &Connection {
        // SAFETY: the connection outlives its requests.
        unsafe { &*self.connection }
    }

    fn bytes(&self, span: Span) -> &[u8] {
        &self.connection().buffer[span.start as usize..span.end as usize]
    }

    /// Text from the head, which was checked to be UTF-8 when it was parsed.
    fn text(&self, span: Span) -> &str {
        // SAFETY: spans in the head lie within the validated text, at character boundaries
        // (they start and end at ASCII delimiters or the head's ends).
        unsafe { std::str::from_utf8_unchecked(self.bytes(span)) }
    }

    pub(crate) fn method(&self) -> &str {
        match self.method {
            Method::Known(name) => name,
            Method::Other(span) => self.text(span),
        }
    }

    fn path(&self) -> &str {
        match &self.path {
            Path::Raw(span) => self.text(*span),
            Path::Decoded(text, _) => text,
        }
    }

    /// The path as sent, still percent-encoded.
    fn raw_path(&self) -> &str {
        match &self.path {
            Path::Raw(span) | Path::Decoded(_, span) => self.text(*span),
        }
    }

    pub(crate) fn query(&self) -> &str {
        self.text(self.query)
    }

    pub(crate) fn remote(&self) -> String {
        self.connection().peer.to_string()
    }

    fn header_list(&self) -> impl Iterator<Item = &Header> {
        self.headers[..self.header_count.min(INLINE_HEADERS)].iter().chain(&self.more_headers)
    }

    /// The headers, names lower-cased, for a WebSocket to keep after the request is gone.
    pub(crate) fn headers_owned(&self) -> Vec<(String, String)> {
        self.header_list().map(|h| (self.text(h.name).to_ascii_lowercase(), self.text(h.value).to_string())).collect()
    }

    /// The first header with this name (case-insensitively).
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.header_list()
            .find(|h| self.bytes(h.name).eq_ignore_ascii_case(name.as_bytes()))
            .map(|h| self.text(h.value))
    }

    pub(crate) fn body(&self) -> &[u8] {
        match &self.body {
            Body::InBuffer(span) => self.bytes(*span),
            Body::Owned(bytes) => bytes,
        }
    }

    /// For a WebSocket upgrade: the connection's socket and the bytes already read past the
    /// request. Responses still waiting to be written go first.
    pub(crate) fn take_connection(&mut self) -> Option<(Arc<Socket>, Vec<u8>)> {
        // SAFETY: the connection outlives the handler call.
        let connection = unsafe { self.connection.as_mut()? };
        if !connection.flush() {
            return None;
        }
        // Only what is left, in a buffer of exactly that size: usually nothing.
        let leftover = connection.buffer[self.end..].to_vec();
        // The connection lives as long as the WebSocket (the handler runs it), but needs only
        // this request's bytes, which the handler may still read.
        connection.buffer.truncate(self.end);
        connection.buffer.shrink_to_fit();
        Some((connection.socket.clone(), leftover))
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

/// Responses waiting in the output past this size are written before the next request is read.
const FLUSH_AT: usize = 16 * 1024;

/// Buffers larger than this are given back once they are empty, so that a connection that once
/// carried a large request or response does not keep the memory while idle.
const KEEP_CAPACITY: usize = 64 * 1024;

struct Connection {
    socket: Arc<Socket>,
    peer: std::net::SocketAddr,
    /// Bytes read from the socket. `start` is the first byte not yet consumed by a request.
    buffer: Vec<u8>,
    start: usize,
    /// How far the search for the end of the next request's head has got.
    scanned: usize,
    /// Responses not yet written. They are written when the connection is about to wait for
    /// more input, so pipelined requests' responses share one `send`.
    out: Vec<u8>,
}

unsafe extern "C" {
    fn memchr(s: *const std::ffi::c_void, c: i32, n: usize) -> *const std::ffi::c_void;
}

/// The position of the first `\n` in `bytes`.
fn find_newline(bytes: &[u8]) -> Option<usize> {
    // SAFETY: searching within the slice.
    let at = unsafe { memchr(bytes.as_ptr().cast(), i32::from(b'\n'), bytes.len()) };
    (!at.is_null()).then(|| at as usize - bytes.as_ptr() as usize)
}

/// Whether `haystack` contains `needle` (lower-case ASCII), ignoring ASCII case.
fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    haystack.as_bytes().windows(needle.len()).any(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
}

impl Connection {
    /// Write the responses waiting in the output. False if the connection is broken.
    fn flush(&mut self) -> bool {
        if self.out.is_empty() {
            return true;
        }
        let written = self.socket.write_all(&self.out).is_ok();
        self.out.clear();
        if self.out.capacity() > KEEP_CAPACITY {
            self.out = Vec::new();
        }
        written
    }

    /// Read more bytes into the buffer. With `idle`, waits up to a minute for the next request
    /// and notices a shutdown. Otherwise this is the middle of a request, which must all arrive
    /// by `request_deadline`, set the first time it is needed. Responses waiting in the output
    /// are written first: the peer may be waiting for them.
    fn fill(&mut self, idle: bool, request_deadline: &mut Option<u64>) -> Result<(), ReadError> {
        if !self.flush() {
            return Err(ReadError::Closed);
        }
        // Read straight into the buffer's spare room, which starts small: an idle connection
        // should cost little memory.
        if self.buffer.capacity() - self.buffer.len() < 1024 {
            self.buffer.reserve(self.buffer.len().max(4096));
        }
        let spare = self.buffer.spare_capacity_mut();
        // SAFETY: the bytes are only read after `read` has written them, and `set_len` covers
        // only those.
        let room = unsafe { std::slice::from_raw_parts_mut(spare.as_mut_ptr() as *mut u8, spare.len()) };
        let deadline = if idle {
            Deadline::After(IDLE_TIMEOUT)
        } else {
            Deadline::Coarse(*request_deadline.get_or_insert_with(|| poll::deadline_after(REQUEST_TIMEOUT)))
        };
        match self.socket.read(room, deadline, idle) {
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

    /// Drop the bytes of requests already served. Only between requests: a live request's
    /// spans point into the buffer.
    fn compact(&mut self) {
        if self.start == 0 {
            return;
        }
        if self.start >= self.buffer.len() {
            self.buffer.clear();
            if self.buffer.capacity() > KEEP_CAPACITY {
                self.buffer = Vec::new();
            }
        } else {
            self.buffer.copy_within(self.start.., 0);
            self.buffer.truncate(self.buffer.len() - self.start);
        }
        self.scanned = self.scanned.saturating_sub(self.start);
        self.start = 0;
    }

    /// The end of the next request's head (the offset of its blank line's `\r\n\r\n`), if it
    /// has all arrived. Resumes where the last search stopped.
    fn find_head_end(&mut self) -> Option<usize> {
        let mut at = self.scanned.max(self.start + 3);
        while at < self.buffer.len() {
            let Some(found) = find_newline(&self.buffer[at..]) else { break };
            let newline = at + found;
            if &self.buffer[newline - 3..=newline] == b"\r\n\r\n" {
                return Some(newline - 3);
            }
            at = newline + 1;
        }
        self.scanned = self.buffer.len();
        None
    }

    /// The next request, or why there is none. Never inlined: its locals would otherwise stay
    /// in `serve_connection`'s frame, which is on the stack for the connection's whole life.
    #[inline(never)]
    fn read_request(&mut self) -> Result<Request, ReadError> {
        self.compact();
        // Idle for at most a minute; then the whole request within 30 seconds of the first
        // wait for more of it. No clock is read unless the connection has to wait.
        let mut deadline = None;
        let header_end = loop {
            if let Some(at) = self.find_head_end() {
                break at;
            }
            if self.buffer.len() > MAX_HEADER {
                return Err(ReadError::Status(431, "Request Header Fields Too Large"));
            }
            let idle = self.buffer.is_empty();
            self.fill(idle, &mut deadline)?;
        };
        let head = std::str::from_utf8(&self.buffer[self.start..header_end])
            .map_err(|_| ReadError::Status(400, "Bad Request"))?;
        let base = self.start;
        let span_of = |part: &str| {
            let at = part.as_ptr() as usize - head.as_ptr() as usize + base;
            Span::new(at, at + part.len())
        };
        let (request_line, header_text) = head.split_once("\r\n").unwrap_or((head, ""));
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
        let mut headers = [Header::default(); INLINE_HEADERS];
        let mut more_headers = Vec::new();
        let mut header_count = 0;
        let (mut connection, mut transfer_encoding, mut content_length, mut expect) = (None, None, None, None);
        for line in header_text.split("\r\n").filter(|_| !header_text.is_empty()) {
            let Some((name, value)) = line.split_once(':') else {
                return Err(ReadError::Status(400, "Bad Request"));
            };
            if name.is_empty() || name.ends_with(' ') || name.ends_with('\t') {
                return Err(ReadError::Status(400, "Bad Request"));
            }
            let value = value.trim();
            // The first of each, as a lookup by name would find.
            let slot = match name.len() {
                10 if name.eq_ignore_ascii_case("connection") => Some(&mut connection),
                17 if name.eq_ignore_ascii_case("transfer-encoding") => Some(&mut transfer_encoding),
                14 if name.eq_ignore_ascii_case("content-length") => Some(&mut content_length),
                6 if name.eq_ignore_ascii_case("expect") => Some(&mut expect),
                _ => None,
            };
            if let Some(slot) = slot
                && slot.is_none()
            {
                *slot = Some(value);
            }
            let header = Header { name: span_of(name), value: span_of(value) };
            if header_count < INLINE_HEADERS {
                headers[header_count] = header;
            } else {
                more_headers.push(header);
            }
            header_count += 1;
        }
        let keep_alive = match connection {
            Some(c) if contains_ignore_case(c, "close") => false,
            Some(c) if contains_ignore_case(c, "keep-alive") => true,
            _ => !http10,
        };
        let chunked = transfer_encoding.is_some_and(|v| contains_ignore_case(v, "chunked"));
        let length = match content_length {
            Some(text) if !chunked => Some(text.parse::<usize>().map_err(|_| ReadError::Status(400, "Bad Request"))?),
            _ => None,
        };
        let max_body = MAX_BODY.load(Ordering::Relaxed) as usize;
        if length.is_some_and(|n| n > max_body) {
            return Err(ReadError::Status(413, "Content Too Large"));
        }
        let expects_continue = expect.is_some_and(|v| v.eq_ignore_ascii_case("100-continue"));
        let method = match METHODS.iter().find(|m| **m == method) {
            Some(known) => Method::Known(known),
            None => Method::Other(span_of(method)),
        };
        let (raw_path, query) = target.split_once('?').unwrap_or((target, ""));
        let path = if raw_path.contains('%') {
            Path::Decoded(percent_decode(raw_path, false).unwrap_or_else(|| raw_path.to_string()), span_of(raw_path))
        } else {
            Path::Raw(span_of(raw_path))
        };
        let query = if query.is_empty() { Span::default() } else { span_of(query) };
        let body_start = header_end + 4;
        if expects_continue && (chunked || length.is_some_and(|n| n > 0)) {
            if !self.flush() {
                return Err(ReadError::Closed);
            }
            let _ = self.socket.write_all(b"HTTP/1.1 100 Continue\r\n\r\n");
        }
        // Reading the body may grow (and so move) the buffer; the spans above are offsets.
        let (body, end) = if chunked {
            let (body, end) = self.read_chunked(body_start, max_body, &mut deadline)?;
            (Body::Owned(body), end)
        } else {
            let length = length.unwrap_or(0);
            while self.buffer.len() < body_start + length {
                self.fill(false, &mut deadline)?;
            }
            (Body::InBuffer(Span::new(body_start, body_start + length)), body_start + length)
        };
        Ok(Request {
            head: matches!(method, Method::Known("HEAD")),
            method,
            path,
            query,
            headers,
            header_count,
            more_headers,
            body,
            responded: false,
            out_start: self.out.len(),
            end,
            keep_alive,
            connection: self,
            upgraded: None,
            file: None,
            stream: None,
            route: metrics::UNMATCHED,
            route_flags: 0,
            status: 0,
            bytes: 0,
        })
    }

    /// The line ending at the next `\r\n` from `at`, waiting for it; returns its end.
    fn line_end(&mut self, at: usize, deadline: &mut Option<u64>) -> Result<usize, ReadError> {
        loop {
            if let Some(found) = self.buffer.get(at..).and_then(|rest| rest.windows(2).position(|w| w == b"\r\n")) {
                return Ok(at + found);
            }
            self.fill(false, deadline)?;
        }
    }

    /// A chunked body starting at `at`: the decoded bytes, and where the request ends.
    fn read_chunked(
        &mut self,
        mut at: usize,
        max_body: usize,
        deadline: &mut Option<u64>,
    ) -> Result<(Vec<u8>, usize), ReadError> {
        let mut body = Vec::new();
        loop {
            let line_end = self.line_end(at, deadline)?;
            let size_text =
                std::str::from_utf8(&self.buffer[at..line_end]).map_err(|_| ReadError::Status(400, "Bad Request"))?;
            let size_text = size_text.split(';').next().unwrap_or_default().trim();
            let size = usize::from_str_radix(size_text, 16).map_err(|_| ReadError::Status(400, "Bad Request"))?;
            at = line_end + 2;
            if size == 0 {
                // Trailers, ending with an empty line.
                loop {
                    let end = self.line_end(at, deadline)?;
                    let empty = end == at;
                    at = end + 2;
                    if empty {
                        return Ok((body, at));
                    }
                }
            }
            if body.len() + size > max_body {
                return Err(ReadError::Status(413, "Content Too Large"));
            }
            while self.buffer.len() < at + size + 2 {
                self.fill(false, deadline)?;
            }
            body.extend_from_slice(&self.buffer[at..at + size]);
            at += size + 2;
        }
    }
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

/// A path segment, percent-decoded; `None` for a bad escape or bytes that are not UTF-8.
pub(crate) fn percent_decode_path(text: &str) -> Option<String> {
    percent_decode(text, false)
}

/// `%XX` for every byte `keep` refuses, upper-case hex.
fn percent_encode(text: &str, keep: impl Fn(u8) -> bool) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(text.len());
    for &b in text.as_bytes() {
        if keep(b) {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 15) as usize] as char);
        }
    }
    out
}

/// RFC 3986's unreserved characters, which `url_encode` leaves as they are.
fn unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')
}

/// What a cookie value may hold unescaped (RFC 6265's `cookie-octet`), less `%`, which starts
/// an escape.
fn cookie_octet(b: u8) -> bool {
    matches!(b, 0x21 | 0x23..=0x24 | 0x26..=0x2B | 0x2D..=0x3A | 0x3C..=0x5B | 0x5D..=0x7E)
}

/// `http.url_encode`: every byte but the unreserved characters as `%XX`.
///
/// # Safety
/// `ptr` and `len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_http_url_encode(ptr: *const u8, len: i64) -> Str {
    let text = unsafe { Str::from_raw(ptr, len) };
    Str::from_bytes(percent_encode(text, unreserved).as_bytes())
}

/// `http.url_decode`: `%XX` escapes decoded, and `+` as a space when `plus_is_space` is not 0;
/// `None` if an escape is malformed or the result is not UTF-8.
///
/// # Safety
/// `ptr` and `len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_http_url_decode(ptr: *const u8, len: i64, plus_is_space: i64) -> Str {
    let text = unsafe { Str::from_raw(ptr, len) };
    optional(percent_decode(text, plus_is_space != 0).as_deref())
}

/// A cookie value as `Set-Cookie` may carry it: bytes it cannot hold, and `%`, as `%XX`.
///
/// # Safety
/// `ptr` and `len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_http_cookie_encode(ptr: *const u8, len: i64) -> Str {
    let text = unsafe { Str::from_raw(ptr, len) };
    Str::from_bytes(percent_encode(text, cookie_octet).as_bytes())
}

/// The first parameter called `name` in a query string, decoded: for test requests, whose
/// query string is not in a connection's buffer.
///
/// # Safety
/// Both pointer and length pairs describe UTF-8 strings.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_http_query_value(
    query_ptr: *const u8,
    query_len: i64,
    name_ptr: *const u8,
    name_len: i64,
) -> Str {
    let query = unsafe { Str::from_raw(query_ptr, query_len) };
    let name = unsafe { Str::from_raw(name_ptr, name_len) };
    optional(query_value(query, name).as_deref())
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

/// Append an HTTP date, `Wed, 18 Nov 2026 12:00:00 GMT`, formatted once a second per thread.
fn push_http_date(out: &mut Vec<u8>) {
    thread_local! {
        static CACHE: std::cell::Cell<(u64, [u8; 29])> = const { std::cell::Cell::new((u64::MAX, [0; 29])) };
    }
    let now = poll::coarse_unix_seconds();
    // Read and written within this call: a task can move to another thread only when it parks.
    let (second, mut text) = CACHE.get();
    if second != now {
        text.copy_from_slice(format_http_date(now).as_bytes());
        CACHE.set((now, text));
    }
    out.extend_from_slice(&text);
}

/// Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
pub(crate) fn civil(days: i64) -> (i64, u32, u32) {
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

pub(crate) fn format_http_date(seconds: u64) -> String {
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

/// A server TLS session for a new connection, if the listener serves HTTPS. Never inlined:
/// with LTO, rustls's set-up was inlined into `serve_connection`, whose frame stays on every
/// connection's stack, and made it 5.9 KiB instead of 3.3 (docs/08, D88).
#[inline(never)]
fn start_tls(tls: Option<Arc<rustls::ServerConfig>>) -> Result<Option<Session>, ()> {
    tls.map(|config| Session::server(&config)).transpose().map_err(|_| ())
}

fn serve_connection(
    stream: TcpStream,
    tls: Option<Arc<rustls::ServerConfig>>,
    peer: std::net::SocketAddr,
    entry: Entry,
    roots: *mut *mut u8,
) {
    // The TLS handshake happens as the first request is read.
    let Ok(session) = start_tls(tls) else { return };
    let mut connection = Connection {
        socket: Arc::new(Socket::with_tls(stream, session)),
        peer,
        buffer: Vec::new(),
        start: 0,
        scanned: 0,
        out: Vec::new(),
    };
    loop {
        let mut request = match connection.read_request() {
            Ok(request) => request,
            Err(ReadError::Closed) => break,
            Err(ReadError::Status(status, text)) => {
                if metrics::on() {
                    metrics::request_failed(match status {
                        408 => metrics::ErrorKind::Timeout,
                        413 | 431 => metrics::ErrorKind::TooLarge,
                        _ => metrics::ErrorKind::Parse,
                    });
                }
                if connection.flush() {
                    write_simple(&connection.socket, status, text);
                }
                break;
            }
        };
        // SAFETY: `roots` holds the function value for the server's lifetime.
        let (code, env) = unsafe { (roots.read(), roots.add(1).read()) };
        request.keep_alive = request.keep_alive && !SHUTDOWN.load(Ordering::SeqCst);
        let handle = &mut request as *mut Request as i64;
        // The clock is read only when metrics are on.
        let started = metrics::on().then(|| {
            metrics::request_started();
            Instant::now()
        });
        // A panicking handler ends only this request, with a 500; the panic was reported
        // where it happened. Mutexes it held are released.
        let handled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| entry(code, env, handle)));
        if let Some(started) = started {
            record(&request, started, handled.is_err());
        }
        if handled.is_err() {
            if let Some(me) = fiber::current() {
                fiber::release_held(me);
            }
            crate::log::clear_request_id();
            request.responded = false;
        }
        if let Some(id) = request.upgraded {
            // The handler has run the WebSocket to its end; the connection is finished.
            crate::ws::forget(id);
            break;
        }
        if !request.responded {
            request.stream = None;
            connection.out.truncate(request.out_start);
            let body = b"internal server error\n";
            write_response(&mut connection.out, 500, "text/plain; charset=utf-8", "", body, false, request.head);
            request.keep_alive = false;
        }
        if let Some(part) = request.stream.take() {
            if !connection.flush() || !send_part(&connection.socket, &part) {
                break;
            }
        }
        connection.start = request.end;
        let keep_alive = request.keep_alive;
        drop(request);
        if !keep_alive {
            connection.flush();
            break;
        }
        if connection.out.len() >= FLUSH_AT && !connection.flush() {
            break;
        }
    }
    connection.socket.shutdown();
}

/// Record a request in the metrics: its route, method, status, time and response size.
#[inline(never)]
fn record(request: &Request, started: Instant, panicked: bool) {
    let elapsed = started.elapsed();
    if panicked {
        metrics::request_failed(metrics::ErrorKind::Panic);
    }
    let (status, bytes) = if panicked || !request.responded && request.upgraded.is_none() {
        (500, 22)
    } else if request.upgraded.is_some() {
        (101, 0)
    } else {
        (request.status, request.bytes)
    };
    if request.route_flags & ROUTE_UNMETERED != 0 {
        // A route left out of the metrics, such as the metrics' own.
        metrics::request_done_unmetered();
        return;
    }
    let method = match request.method {
        Method::Known(name) => metrics::method_index(name),
        Method::Other(_) => 7,
    };
    let elapsed = if request.upgraded.is_some() { None } else { Some(elapsed) };
    if crate::dashboard::events_on() {
        let nanos = elapsed.map(|e| e.as_nanos().min(u128::from(u64::MAX)) as u64);
        if crate::dashboard::qualifies(status, nanos) {
            note_event(request, status, nanos);
        }
    }
    metrics::request_finished(request.route, method, status, elapsed, bytes);
}

/// Keep a server error or slow request for the dashboard, with its request ID: from the
/// `X-Request-Id` the response carries (`request_log` has forgotten it by now), else the
/// task's, which a handler that panicked leaves behind.
#[cold]
#[inline(never)]
fn note_event(request: &Request, status: i64, nanos: Option<u64>) {
    let method = match request.method {
        Method::Known(name) => name,
        Method::Other(span) => request.text(span),
    };
    let out = &request.connection().out;
    let head = out.get(request.out_start..).unwrap_or(&[]);
    let head = &head[..head.windows(4).position(|w| w == b"\r\n\r\n").unwrap_or(head.len())];
    let sent = head.split(|&b| b == b'\n').find_map(|line| {
        let (name, value) = line.split_at(line.iter().position(|&b| b == b':')?);
        name.eq_ignore_ascii_case(b"x-request-id").then(|| String::from_utf8_lossy(&value[1..]).trim().to_string())
    });
    let id = sent.or_else(|| crate::log::current_request_id().map(|id| id.to_string())).unwrap_or_default();
    crate::dashboard::note(method, request.route, request.path(), status, nanos, id);
}

/// A route left out of the metrics, such as the metrics' own.
const ROUTE_UNMETERED: u8 = 1;
// Flag 2 marks a route left out of the access log, such as the health checks; only the http
// module reads it.

/// Which route took the request: its metrics slot plus its flags times 65536
/// ([`ROUTE_UNMETERED`], and 2 for a route left out of the access log). One store; test requests (a negative handle) keep it
/// themselves.
///
/// # Safety
/// `handle` is a live request, or negative.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_http_set_route(handle: i64, route: i64) {
    if handle < 0 {
        return;
    }
    let request = unsafe { request(handle) };
    request.route = (route & 0xffff).min(i64::from(metrics::UNMATCHED)) as u16;
    request.route_flags = ((route >> 16) & 0xff) as u8;
}

/// What `lrt_http_set_route` was given, or the unmatched slot.
///
/// # Safety
/// `handle` is a live request.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_http_req_route(handle: i64) -> i64 {
    let request = unsafe { request(handle) };
    i64::from(request.route) | (i64::from(request.route_flags) << 16)
}

/// The pattern of a route's slot (flags ignored), or `""` for the unmatched slot.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_http_route_pattern(route: i64) -> Str {
    match u16::try_from(route & 0xffff).ok().and_then(metrics::route_pattern) {
        Some(pattern) => Str { ptr: pattern.as_ptr(), len: pattern.len() as i64 },
        None => Str::empty(),
    }
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
    match unsafe { request(handle) }.method {
        // A static string, like the program's own string constants: nothing to copy.
        Method::Known(name) => Str { ptr: name.as_ptr(), len: name.len() as i64 },
        Method::Other(_) => Str::from_bytes(unsafe { request(handle) }.method().as_bytes()),
    }
}

/// # Safety
/// `handle` is a live request.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_http_req_path(handle: i64) -> Str {
    Str::from_bytes(unsafe { request(handle) }.path().as_bytes())
}

/// The query string as sent, without the `?`: `""` if there is none.
///
/// # Safety
/// `handle` is a live request.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_http_req_query_string(handle: i64) -> Str {
    Str::from_bytes(unsafe { request(handle) }.query().as_bytes())
}

/// # Safety
/// `handle` is a live request.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_http_req_remote(handle: i64) -> Str {
    Str::from_bytes(unsafe { request(handle) }.remote().as_bytes())
}

/// The first query parameter with this name, decoded, or `None`.
///
/// # Safety
/// `handle` is a live request; `ptr` and `len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_http_req_query(handle: i64, ptr: *const u8, len: i64) -> Str {
    let wanted = unsafe { Str::from_raw(ptr, len) };
    optional(query_value(unsafe { request(handle) }.query(), wanted).as_deref())
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
    let wanted = unsafe { Str::from_raw(ptr, len) };
    optional(unsafe { request(handle) }.header(wanted))
}

/// The body as text; `None` if it is not valid UTF-8.
///
/// # Safety
/// `handle` is a live request.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_http_req_body(handle: i64) -> Str {
    optional(std::str::from_utf8(unsafe { request(handle) }.body()).ok())
}

/// The body's bytes, whether or not they are text (the data of a `bytes` value).
///
/// # Safety
/// `handle` is a live request.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_http_req_body_bytes(handle: i64) -> Str {
    Str::from_bytes(unsafe { request(handle) }.body())
}

/// The client's IP address: the peer's, or, when the peer is a trusted proxy (see
/// [`lrt_http_set_trusted_proxies`]), the address it forwarded the request for.
///
/// # Safety
/// `handle` is a live request.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_http_req_client_ip(handle: i64) -> Str {
    let request = unsafe { request(handle) };
    let peer = request.connection().peer.ip().to_canonical();
    let trusted = TRUSTED_PROXIES.read().unwrap_or_else(|p| p.into_inner());
    let forwarded = request
        .header_list()
        .filter(|h| request.bytes(h.name).eq_ignore_ascii_case(b"x-forwarded-for"))
        .map(|h| request.text(h.value));
    Str::from_bytes(client_ip(peer, forwarded, &trusted).to_string().as_bytes())
}

/// Whether what this request carries stayed off the open network: it came over TLS, from this
/// machine, or through a trusted proxy whose `X-Forwarded-Proto` says the client used HTTPS.
///
/// # Safety
/// `handle` is a live request.
pub(crate) unsafe fn request_is_private(handle: i64) -> bool {
    let request = unsafe { request(handle) };
    let connection = request.connection();
    if connection.socket.tls.is_some() {
        return true;
    }
    let peer = connection.peer.ip().to_canonical();
    let trusted = TRUSTED_PROXIES.read().unwrap_or_else(|p| p.into_inner());
    if trusted.iter().any(|n| n.contains(peer)) {
        return request.header_list().any(|h| {
            request.bytes(h.name).eq_ignore_ascii_case(b"x-forwarded-proto")
                && request.text(h.value).trim().eq_ignore_ascii_case("https")
        });
    }
    peer.is_loopback()
}

/// Addresses and networks whose `X-Forwarded-For` is believed, for every server in the process.
static TRUSTED_PROXIES: std::sync::RwLock<Vec<Network>> = std::sync::RwLock::new(Vec::new());

/// An address with a prefix length: `10.0.0.0/8`, or `127.0.0.1` for `/32`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Network {
    address: std::net::IpAddr,
    prefix: u8,
}

impl Network {
    fn parse(text: &str) -> Option<Network> {
        let (address, prefix) = match text.split_once('/') {
            Some((address, prefix)) => (address, Some(prefix)),
            None => (text, None),
        };
        let address: std::net::IpAddr = address.trim().parse().ok()?;
        let address = address.to_canonical();
        let bits = if address.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            Some(p) => p.trim().parse::<u8>().ok().filter(|p| *p <= bits)?,
            None => bits,
        };
        Some(Network { address, prefix })
    }

    fn contains(&self, address: std::net::IpAddr) -> bool {
        use std::net::IpAddr;
        match (self.address, address.to_canonical()) {
            (IpAddr::V4(net), IpAddr::V4(a)) => {
                let mask = if self.prefix == 0 { 0 } else { u32::MAX << (32 - self.prefix) };
                u32::from(net) & mask == u32::from(a) & mask
            }
            (IpAddr::V6(net), IpAddr::V6(a)) => {
                let mask = if self.prefix == 0 { 0 } else { u128::MAX << (128 - self.prefix) };
                u128::from(net) & mask == u128::from(a) & mask
            }
            _ => false,
        }
    }
}

/// The client behind `peer`: walking `X-Forwarded-For` from the right (the nearest hop) past
/// trusted proxies, the first address that is not one. An entry that is not an address ends
/// the walk at the hop before it, which is the last one known.
fn client_ip<'a>(
    peer: std::net::IpAddr,
    forwarded: impl Iterator<Item = &'a str>,
    trusted: &[Network],
) -> std::net::IpAddr {
    if !trusted.iter().any(|n| n.contains(peer)) {
        return peer;
    }
    let hops: Vec<&str> =
        forwarded.flat_map(|value| value.split(',')).map(str::trim).filter(|h| !h.is_empty()).collect();
    let mut client = peer;
    for hop in hops.iter().rev() {
        let Some(address) = parse_hop(hop) else { break };
        client = address;
        if !trusted.iter().any(|n| n.contains(address)) {
            break;
        }
    }
    client
}

/// An address in `X-Forwarded-For`: plain, or with a port (`1.2.3.4:80`, `[::1]:80`).
fn parse_hop(hop: &str) -> Option<std::net::IpAddr> {
    if let Ok(address) = hop.parse::<std::net::IpAddr>() {
        return Some(address.to_canonical());
    }
    hop.parse::<std::net::SocketAddr>().ok().map(|s| s.ip().to_canonical())
}

/// Trust these proxies' `X-Forwarded-For`: addresses or networks separated by commas. Returns
/// `""`, or the first entry that is neither, leaving the list as it was.
///
/// # Safety
/// `ptr` and `len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_http_set_trusted_proxies(ptr: *const u8, len: i64) -> Str {
    let text = unsafe { Str::from_raw(ptr, len) };
    let mut networks = Vec::new();
    for entry in text.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        match Network::parse(entry) {
            Some(network) => networks.push(network),
            None => return Str::from_bytes(entry.as_bytes()),
        }
    }
    *TRUSTED_PROXIES.write().unwrap_or_else(|p| p.into_inner()) = networks;
    Str::empty()
}

// ---- static files ------------------------------------------------------------------------

/// Files up to this size are read straight into the connection's output; larger ones are sent
/// in [`STREAM_CHUNK`]s after the handler returns, so a large download never holds the file.
const SMALL_FILE: u64 = 1024 * 1024;
const STREAM_CHUNK: usize = 256 * 1024;

/// Check `server.static`'s root: `""` if it is a directory, otherwise what is wrong.
///
/// # Safety
/// `ptr` and `len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_http_static_root(ptr: *const u8, len: i64) -> Str {
    let root = unsafe { Str::from_raw(ptr, len) };
    let message = match std::fs::metadata(root) {
        Ok(meta) if meta.is_dir() => String::new(),
        Ok(_) => "is not a directory".to_string(),
        Err(error) => format!("cannot be read: {error}"),
    };
    Str::from_bytes(message.as_bytes())
}

/// The path below a prefix of `depth` segments, without its leading `/`.
fn below(raw: &str, depth: i64) -> &str {
    let mut rest = raw.strip_prefix('/').unwrap_or(raw);
    for _ in 0..depth {
        rest = rest.split_once('/').map_or("", |(_, after)| after);
    }
    rest
}

/// Answer a request for `server.static`. `settings` is the root, the index file and the
/// fallback file, `conditions` the values of `If-Match`, `If-None-Match`, `If-Modified-Since`,
/// `If-Unmodified-Since`, `Range`, `If-Range` and `Accept-Encoding` (`""` when absent), each
/// followed by a NUL. `flags`: 1 for dotfiles, 2 for immutable.
///
/// Returns `status\ncontent type\nheader lines` and a NUL. For a request from a connection the
/// file is kept with the request for `lrt_http_respond`; for a test request (`handle` < 0,
/// with `path` its path) the bytes to send follow the NUL.
///
/// # Safety
/// `handle` is a live request or negative; the strings are UTF-8.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C-unwind" fn lrt_http_static(
    handle: i64,
    path: *const u8,
    path_len: i64,
    depth: i64,
    settings: *const u8,
    settings_len: i64,
    flags: i64,
    max_age: i64,
    conditions: *const u8,
    conditions_len: i64,
) -> Str {
    let (path, settings, conditions) = unsafe {
        (
            Str::from_raw(path, path_len),
            Str::from_raw(settings, settings_len),
            Str::from_raw(conditions, conditions_len),
        )
    };
    let mut settings = settings.split('\0');
    let mut setting = || settings.next().unwrap_or("");
    let options = crate::static_files::Options {
        root: setting(),
        index: setting(),
        fallback: setting(),
        dotfiles: flags & 1 != 0,
        max_age,
        immutable: flags & 2 != 0,
    };
    let mut values = conditions.split('\0');
    let mut value = || values.next().filter(|v| !v.is_empty());
    let conditions = crate::static_files::Conditions {
        if_match: value(),
        if_none_match: value(),
        if_modified_since: value(),
        if_unmodified_since: value(),
        range: value(),
        if_range: value(),
        accept_encoding: value(),
    };
    let mut request = (handle >= 0).then(|| unsafe { request(handle) });
    let raw = request.as_ref().map_or(path, |r| r.raw_path());
    let outcome = crate::static_files::resolve(below(raw, depth), raw, &options, &conditions);
    let mut out = Vec::new();
    match outcome {
        crate::static_files::Outcome::NotFound => out.extend_from_slice(b"404\n\n\0"),
        crate::static_files::Outcome::Respond { status, content_type, headers, body } => {
            out.extend_from_slice(format!("{status}\n{content_type}\n{headers}\0").as_bytes());
            if let Some(part) = body {
                match request.as_mut() {
                    Some(request) => request.file = Some((i64::from(status), part)),
                    None => {
                        let start = out.len();
                        out.resize(start + part.len as usize, 0);
                        if read_part(&part, &mut out[start..], part.start).is_err() {
                            out.truncate(start);
                        }
                    }
                }
            }
        }
    }
    Str::from_bytes(&out)
}

/// Read `buffer.len()` bytes of the file at `offset`.
fn read_part(part: &crate::static_files::Part, buffer: &mut [u8], offset: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(&part.file, buffer, offset)
}

/// Send a large file's part to the socket, a chunk at a time; the task parks while the socket
/// is full. False if the connection broke or the file shrank, which ends the connection, since
/// the length was promised.
fn send_part(socket: &Socket, part: &crate::static_files::Part) -> bool {
    let mut chunk = vec![0u8; STREAM_CHUNK.min(part.len as usize)];
    let mut sent = 0;
    while sent < part.len {
        let n = chunk.len().min((part.len - sent) as usize);
        if read_part(part, &mut chunk[..n], part.start + sent).is_err() || socket.write_all(&chunk[..n]).is_err() {
            return false;
        }
        sent += n as u64;
    }
    true
}

/// Leave the response: its status, content type (none if empty), other header lines
/// (`Name: value\r\n` each) and body. It is written into the connection's output at once,
/// while the body is reachable, so the body is copied once. A second call replaces the first.
///
/// # Safety
/// `handle` is a live request; the content type and headers are UTF-8, and the body any bytes.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C-unwind" fn lrt_http_respond(
    handle: i64,
    status: i64,
    content_type: *const u8,
    content_type_len: i64,
    headers: *const u8,
    headers_len: i64,
    body: *const u8,
    body_len: i64,
) {
    let content_type = unsafe { Str::from_raw(content_type, content_type_len) };
    let headers = unsafe { Str::from_raw(headers, headers_len) };
    // The body may be `bytes`, so it is not taken as text.
    let body: &[u8] = if body_len <= 0 { &[] } else { unsafe { std::slice::from_raw_parts(body, body_len as usize) } };
    let request = unsafe { request(handle) };
    // SAFETY: the connection outlives the handler call.
    let out = unsafe { &mut (*request.connection).out };
    out.truncate(request.out_start);
    request.stream = None;
    request.responded = true;
    request.status = status;
    // Body bytes sent: none for HEAD and 304.
    let sent = |len: u64| if request.head || status == 304 { 0 } else { len };
    request.bytes = sent(body.len() as u64);
    // A file from `lrt_http_static`, unless middleware put another response in its place.
    if let Some((_, part)) = request.file.take().filter(|(file_status, _)| *file_status == status && body.is_empty()) {
        request.bytes = sent(part.len);
        write_head(out, status, content_type, headers, part.len, request.keep_alive);
        if request.head || status == 304 {
            return;
        }
        if part.len > SMALL_FILE {
            request.stream = Some(part);
            return;
        }
        let start = out.len();
        out.resize(start + part.len as usize, 0);
        if read_part(&part, &mut out[start..], part.start).is_ok() {
            return;
        }
        out.truncate(request.out_start);
        write_response(
            out,
            500,
            "text/plain; charset=utf-8",
            "",
            b"internal server error\n",
            request.keep_alive,
            request.head,
        );
        return;
    }
    write_response(out, status, content_type, headers, body, request.keep_alive, request.head);
}

/// Append a decimal number.
fn push_decimal(out: &mut Vec<u8>, mut value: u64) {
    let mut digits = [0u8; 20];
    let mut at = digits.len();
    loop {
        at -= 1;
        digits[at] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    out.extend_from_slice(&digits[at..]);
}

/// Append a complete HTTP/1.1 response: status line, `Content-Type`, the handler's header
/// lines, the automatic headers, and the body (left out for `HEAD`, 204 and 304).
fn write_response(
    out: &mut Vec<u8>,
    status: i64,
    content_type: &str,
    headers: &str,
    body: &[u8],
    keep_alive: bool,
    head: bool,
) {
    out.reserve(192 + content_type.len() + headers.len() + if head { 0 } else { body.len() });
    write_head(out, status, content_type, headers, body.len() as u64, keep_alive);
    if !head && status != 204 && status != 304 {
        out.extend_from_slice(body);
    }
}

/// The status line and headers of a response whose body is `body_len` bytes.
fn write_head(out: &mut Vec<u8>, status: i64, content_type: &str, headers: &str, body_len: u64, keep_alive: bool) {
    let has_body = status != 204 && status != 304;
    out.extend_from_slice(b"HTTP/1.1 ");
    if status < 0 {
        out.push(b'-');
    }
    push_decimal(out, status.unsigned_abs());
    out.push(b' ');
    out.extend_from_slice(reason(status).as_bytes());
    out.extend_from_slice(b"\r\n");
    if !content_type.is_empty() {
        out.extend_from_slice(b"Content-Type: ");
        out.extend_from_slice(content_type.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(headers.as_bytes());
    out.extend_from_slice(b"Date: ");
    push_http_date(out);
    out.extend_from_slice(b"\r\nServer: ");
    out.extend_from_slice(lang_names::CLI_NAME.as_bytes());
    out.extend_from_slice(b"\r\n");
    if has_body {
        out.extend_from_slice(b"Content-Length: ");
        push_decimal(out, body_len);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(if keep_alive { b"Connection: keep-alive\r\n\r\n" } else { b"Connection: close\r\n\r\n" });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_client_is_the_first_untrusted_hop_from_the_right() {
        let trusted: Vec<Network> =
            ["10.0.0.0/8", "::1", "192.168.1.7"].iter().map(|t| Network::parse(t).unwrap()).collect();
        let ip = |t: &str| t.parse::<std::net::IpAddr>().unwrap();
        let client = |peer: &str, headers: &[&str]| client_ip(ip(peer), headers.iter().copied(), &trusted).to_string();
        // An untrusted peer's header is ignored.
        assert_eq!(client("203.0.113.9", &["1.1.1.1"]), "203.0.113.9");
        // Through one trusted proxy, and through two, across two header lines.
        assert_eq!(client("10.1.2.3", &["198.51.100.4"]), "198.51.100.4");
        assert_eq!(client("10.1.2.3", &["6.6.6.6, 198.51.100.4", "192.168.1.7"]), "198.51.100.4");
        // A client cannot hide behind a forged entry: the rightmost untrusted one wins.
        assert_eq!(client("10.1.2.3", &["1.2.3.4, 198.51.100.4"]), "198.51.100.4");
        // Every hop trusted: the leftmost. Garbage ends the walk at the hop before it.
        assert_eq!(client("10.1.2.3", &["10.9.9.9"]), "10.9.9.9");
        assert_eq!(client("10.1.2.3", &["nonsense"]), "10.1.2.3");
        // Ports, IPv6, and IPv4-mapped IPv6 peers.
        assert_eq!(client("::1", &["[2001:db8::5]:443"]), "2001:db8::5");
        assert_eq!(client("::ffff:10.0.0.1", &["198.51.100.4:8080"]), "198.51.100.4");
        // No header from a trusted proxy: the proxy itself.
        assert_eq!(client("10.1.2.3", &[]), "10.1.2.3");
    }

    #[test]
    fn networks_parse_and_match() {
        assert!(Network::parse("10.0.0.0/33").is_none());
        assert!(Network::parse("10.0.0/8").is_none());
        assert!(Network::parse("fd00::/8").unwrap().contains("fd12::1".parse().unwrap()));
        assert!(Network::parse("0.0.0.0/0").unwrap().contains("8.8.8.8".parse().unwrap()));
        assert!(!Network::parse("10.0.0.0/8").unwrap().contains("::1".parse().unwrap()));
    }

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

    #[test]
    fn percent_encoding() {
        assert_eq!(percent_encode("a b&c=d/é~", unreserved), "a%20b%26c%3Dd%2F%C3%A9~");
        assert_eq!(percent_decode(&percent_encode("a+b %", unreserved), true).as_deref(), Some("a+b %"));
        // Cookie values keep everything a cookie may hold, and escape the rest and `%`.
        assert_eq!(percent_encode("ab.c-_=/:", cookie_octet), "ab.c-_=/:");
        assert_eq!(percent_encode("a b;c,d\"%é", cookie_octet), "a%20b%3Bc%2Cd%22%25%C3%A9");
    }
}
