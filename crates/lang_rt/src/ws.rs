//! WebSockets (RFC 6455) on top of the HTTP server (docs/05-standard-library.md, "WebSockets").
//!
//! A route handler upgrades its request with `lrt_ws_accept`, which checks the handshake headers,
//! writes the `101 Switching Protocols` response, and registers a socket under a new id. The
//! handler then runs for the life of the connection on the connection's own task, receiving with
//! `lrt_ws_recv`; any task may send with `lrt_ws_send` or close with `lrt_ws_close`, since writes
//! are serialised by a lock per socket. When the handler returns, `lrt_ws_finish` completes the
//! closing handshake and the HTTP layer closes the connection.
//!
//! The runtime answers pings, ignores pongs, reassembles fragmented messages, validates UTF-8 in
//! text messages and close reasons, enforces a message size limit, and fails the connection with
//! the status code the RFC asks for when the peer breaks the protocol. A keep-alive ping is sent
//! after a quiet interval, and a peer that stays silent for two intervals is dropped. Extensions
//! (`permessage-deflate`) are not negotiated, so the reserved bits must be zero.
//!
//! Sockets hold only Rust data; messages are copied into collected strings when received.

use crate::fiber::FiberLock;
use crate::{Str, http, sys};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};

const OP_CONTINUATION: u8 = 0x0;
const OP_TEXT: u8 = 0x1;
const OP_BINARY: u8 = 0x2;
const OP_CLOSE: u8 = 0x8;
const OP_PING: u8 = 0x9;
const OP_PONG: u8 = 0xA;

const CLOSE_NORMAL: u16 = 1000;
const CLOSE_GOING_AWAY: u16 = 1001;
const CLOSE_PROTOCOL: u16 = 1002;
const CLOSE_INVALID_DATA: u16 = 1007;
const CLOSE_TOO_BIG: u16 = 1009;

/// What `lrt_ws_recv` returns.
const RECV_CLOSED: i64 = 0;
const RECV_TEXT: i64 = 1;
const RECV_BINARY: i64 = 2;

/// How long to wait for the peer's close frame after sending ours.
const CLOSE_WAIT: Duration = Duration::from_secs(2);

static MAX_MESSAGE: AtomicI64 = AtomicI64::new(16 * 1024 * 1024);
static PING_SECONDS: AtomicI64 = AtomicI64::new(30);
static NEXT_ID: AtomicI64 = AtomicI64::new(1);
static SOCKETS: LazyLock<Mutex<HashMap<i64, Arc<WebSocket>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

thread_local! {
    /// The message the last `lrt_ws_recv` on this thread received, until `lrt_ws_payload`.
    static PAYLOAD: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

/// The locks park fibers: the reader's is held while waiting for the peer, and the writer's
/// while waiting for room to send.
struct WebSocket {
    writer: FiberLock<Writer>,
    reader: FiberLock<Reader>,
    /// From the upgrade request.
    query: String,
    headers: Vec<(String, String)>,
    remote: String,
}

struct Writer {
    socket: Arc<http::Socket>,
    close_sent: bool,
}

impl Writer {
    /// Write one unfragmented, unmasked frame. False if the connection is gone.
    fn frame(&mut self, opcode: u8, payload: &[u8]) -> bool {
        if self.close_sent {
            return false;
        }
        let mut out = Vec::with_capacity(payload.len() + 10);
        out.push(0x80 | opcode);
        match payload.len() {
            n if n < 126 => out.push(n as u8),
            n if n <= 0xFFFF => {
                out.push(126);
                out.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                out.push(127);
                out.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        out.extend_from_slice(payload);
        if opcode == OP_CLOSE {
            self.close_sent = true;
        }
        self.socket.write_all(&out).is_ok()
    }

    fn close(&mut self, code: u16, reason: &[u8]) {
        let mut payload = code.to_be_bytes().to_vec();
        payload.extend_from_slice(&reason[..reason.len().min(123)]);
        self.frame(OP_CLOSE, &payload);
    }
}

struct Reader {
    socket: Arc<http::Socket>,
    buffer: Vec<u8>,
    /// The peer's close frame has arrived, or the connection failed: nothing more to read.
    closed: bool,
    last_heard: Instant,
    ping_sent: bool,
    /// While finishing: stop waiting for the peer at this time.
    deadline: Option<Instant>,
}

struct Frame {
    fin: bool,
    opcode: u8,
    payload: Vec<u8>,
}

/// Why reading stopped.
enum Stop {
    /// The connection is gone, or the server is shutting down: send this close code if any.
    Gone(Option<u16>),
    /// The peer broke the protocol: fail the connection with this code.
    Fail(u16),
}

impl Reader {
    /// Read more bytes, sending keep-alive pings while the peer is quiet.
    fn fill(&mut self, socket: &WebSocket) -> Result<(), Stop> {
        loop {
            let now = Instant::now();
            if self.deadline.is_some_and(|d| now > d) {
                return Err(Stop::Gone(None));
            }
            if http::shutting_down() && self.deadline.is_none() {
                return Err(Stop::Gone(Some(CLOSE_GOING_AWAY)));
            }
            let interval = Duration::from_secs(PING_SECONDS.load(Ordering::Relaxed).max(1) as u64);
            let quiet = now.duration_since(self.last_heard);
            if quiet > interval * 2 {
                return Err(Stop::Gone(Some(CLOSE_GOING_AWAY)));
            }
            if quiet > interval && !self.ping_sent {
                self.ping_sent = true;
                if !socket.writer.lock().frame(OP_PING, b"") {
                    return Err(Stop::Gone(None));
                }
            }
            let mut wake_at = self.last_heard + if self.ping_sent { interval * 2 } else { interval };
            if let Some(deadline) = self.deadline {
                wake_at = wake_at.min(deadline);
            }
            if self.buffer.capacity() - self.buffer.len() < 1024 {
                self.buffer.reserve(self.buffer.len().max(4096));
            }
            let spare = self.buffer.spare_capacity_mut();
            // SAFETY: only the bytes `read` wrote are added to the buffer.
            let room = unsafe { std::slice::from_raw_parts_mut(spare.as_mut_ptr() as *mut u8, spare.len()) };
            match self.socket.read(room, Some(wake_at), self.deadline.is_none()) {
                Ok(0) => return Err(Stop::Gone(None)),
                Ok(n) => {
                    unsafe { self.buffer.set_len(self.buffer.len() + n) };
                    self.last_heard = Instant::now();
                    self.ping_sent = false;
                    return Ok(());
                }
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
                Err(_) => return Err(Stop::Gone(None)),
            }
        }
    }

    /// Ensure `n` bytes are buffered.
    fn need(&mut self, socket: &WebSocket, n: usize) -> Result<(), Stop> {
        while self.buffer.len() < n {
            self.fill(socket)?;
        }
        Ok(())
    }

    /// The next frame, checked against the rules that do not depend on earlier frames.
    fn frame(&mut self, socket: &WebSocket, max: usize) -> Result<Frame, Stop> {
        self.need(socket, 2)?;
        let (b0, b1) = (self.buffer[0], self.buffer[1]);
        let fin = b0 & 0x80 != 0;
        let opcode = b0 & 0x0F;
        if b0 & 0x70 != 0 {
            return Err(Stop::Fail(CLOSE_PROTOCOL)); // reserved bits, without an extension
        }
        if !matches!(opcode, OP_CONTINUATION | OP_TEXT | OP_BINARY | OP_CLOSE | OP_PING | OP_PONG) {
            return Err(Stop::Fail(CLOSE_PROTOCOL));
        }
        if b1 & 0x80 == 0 {
            return Err(Stop::Fail(CLOSE_PROTOCOL)); // a client must mask its frames
        }
        let control = opcode & 0x8 != 0;
        let (length, header) = match b1 & 0x7F {
            126 => {
                self.need(socket, 4)?;
                (u16::from_be_bytes([self.buffer[2], self.buffer[3]]) as u64, 4)
            }
            127 => {
                self.need(socket, 10)?;
                let mut bytes = [0u8; 8];
                bytes.copy_from_slice(&self.buffer[2..10]);
                let length = u64::from_be_bytes(bytes);
                if length >> 63 != 0 {
                    return Err(Stop::Fail(CLOSE_PROTOCOL));
                }
                (length, 10)
            }
            n => (n as u64, 2),
        };
        if control && (length > 125 || !fin) {
            return Err(Stop::Fail(CLOSE_PROTOCOL));
        }
        if length > max as u64 {
            return Err(Stop::Fail(CLOSE_TOO_BIG));
        }
        let length = length as usize;
        self.need(socket, header + 4 + length)?;
        let mask = [self.buffer[header], self.buffer[header + 1], self.buffer[header + 2], self.buffer[header + 3]];
        let start = header + 4;
        let mut payload: Vec<u8> = self.buffer[start..start + length].to_vec();
        for (i, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[i & 3];
        }
        self.buffer.drain(..start + length);
        Ok(Frame { fin, opcode, payload })
    }

    /// The next complete message as (opcode, data), answering control frames on the way.
    fn message(&mut self, socket: &WebSocket) -> Result<(u8, Vec<u8>), Stop> {
        let max = MAX_MESSAGE.load(Ordering::Relaxed).max(125) as usize;
        let mut message: Option<(u8, Vec<u8>)> = None;
        // How much of a fragmented text message is known to be valid UTF-8, so that invalid
        // text fails the connection at the fragment that has it, not at the end (Autobahn 6.4).
        let mut checked = 0;
        loop {
            let frame = self.frame(socket, max)?;
            match frame.opcode {
                OP_PING => {
                    socket.writer.lock().frame(OP_PONG, &frame.payload);
                }
                OP_PONG => {}
                OP_CLOSE => {
                    let reply = close_reply(&frame.payload);
                    self.closed = true;
                    let mut writer = socket.writer.lock();
                    match reply {
                        Ok(code) if !writer.close_sent => writer.close(code, b""),
                        Ok(_) => {}
                        Err(code) => writer.close(code, b""),
                    }
                    return Err(Stop::Gone(None));
                }
                OP_TEXT | OP_BINARY => {
                    if message.is_some() {
                        return Err(Stop::Fail(CLOSE_PROTOCOL)); // a new message inside a fragmented one
                    }
                    message = Some((frame.opcode, frame.payload));
                }
                _ => {
                    let Some((_, data)) = message.as_mut() else {
                        return Err(Stop::Fail(CLOSE_PROTOCOL)); // a continuation of nothing
                    };
                    if data.len() + frame.payload.len() > max {
                        return Err(Stop::Fail(CLOSE_TOO_BIG));
                    }
                    data.extend_from_slice(&frame.payload);
                }
            }
            if !frame.fin
                && let Some((OP_TEXT, data)) = &message
            {
                match std::str::from_utf8(&data[checked..]) {
                    Ok(_) => checked = data.len(),
                    // Only an incomplete character at the end may be finished by the next frame.
                    Err(e) if e.error_len().is_none() => checked += e.valid_up_to(),
                    Err(_) => return Err(Stop::Fail(CLOSE_INVALID_DATA)),
                }
            }
            // A final data frame completes the message (control frames may come in between).
            if frame.fin
                && frame.opcode < OP_CLOSE
                && let Some((opcode, data)) = message.take()
            {
                if opcode == OP_TEXT && std::str::from_utf8(&data[checked..]).is_err() {
                    return Err(Stop::Fail(CLOSE_INVALID_DATA));
                }
                return Ok((opcode, data));
            }
        }
    }
}

/// The code to answer a close frame with: `Ok` to echo, `Err` when the frame itself is invalid.
fn close_reply(payload: &[u8]) -> Result<u16, u16> {
    match payload.len() {
        0 => Ok(CLOSE_NORMAL),
        1 => Err(CLOSE_PROTOCOL),
        _ => {
            let code = u16::from_be_bytes([payload[0], payload[1]]);
            // The codes a peer may send: those defined by RFC 6455 for use in close frames, and
            // the ranges for libraries and applications.
            let valid = matches!(code, 1000..=1003 | 1007..=1011 | 3000..=4999);
            if !valid {
                Err(CLOSE_PROTOCOL)
            } else if std::str::from_utf8(&payload[2..]).is_err() {
                Err(CLOSE_INVALID_DATA)
            } else {
                Ok(code)
            }
        }
    }
}

fn socket(id: i64) -> Option<Arc<WebSocket>> {
    lock(&SOCKETS).get(&id).cloned()
}

// ---- the handshake --------------------------------------------------------------------

/// The `Sec-WebSocket-Accept` value for a key: base64 of the SHA-1 of the key and a fixed GUID.
fn accept_key(key: &str) -> String {
    let mut input = key.as_bytes().to_vec();
    input.extend_from_slice(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    base64(&sha1(&input))
}

fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in message.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([block[i * 4], block[i * 4 + 1], block[i * 4 + 2], block[i * 4 + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, word) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (slot, value) in h.iter_mut().zip([a, b, c, d, e]) {
            *slot = slot.wrapping_add(value);
        }
    }
    let mut out = [0u8; 20];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

pub(crate) const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub(crate) fn base64(data: &[u8]) -> String {
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let n =
            (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(BASE64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The number of bytes a base64 text decodes to, if it is well formed.
fn base64_len(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    if bytes.len() % 4 != 0 {
        return None;
    }
    let padding = bytes.iter().rev().take_while(|&&b| b == b'=').count();
    if padding > 2 || !bytes[..bytes.len() - padding].iter().all(|b| BASE64.contains(b)) {
        return None;
    }
    Some(bytes.len() / 4 * 3 - padding)
}

/// Upgrade the request to a WebSocket. Returns the socket's id; -1 if the request is not a
/// valid WebSocket handshake, -2 if it asks for a protocol version other than 13 (the error is
/// recorded either way).
///
/// # Safety
/// `handle` is a live request.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_ws_accept(handle: i64) -> i64 {
    if handle < 0 {
        sys::record_error("the request did not come from a connection");
        return -1;
    }
    let request = unsafe { http::request(handle) };
    let has_token = |name: &str, token: &str| {
        request.header(name).is_some_and(|v| v.split(',').any(|t| t.trim().eq_ignore_ascii_case(token)))
    };
    if request.method() != "GET" || !has_token("upgrade", "websocket") || !has_token("connection", "upgrade") {
        sys::record_error("expected a WebSocket handshake: a GET with `Upgrade: websocket`");
        return -1;
    }
    if request.header("sec-websocket-version") != Some("13") {
        sys::record_error("unsupported WebSocket version; this server speaks version 13");
        return -2;
    }
    let Some(key) = request.header("sec-websocket-key").filter(|k| base64_len(k) == Some(16)) else {
        sys::record_error("missing or malformed `Sec-WebSocket-Key`");
        return -1;
    };
    let accept = accept_key(key);
    let Some((connection, leftover)) = request.take_connection() else {
        sys::record_error("the connection is not available");
        return -1;
    };
    let response = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
    );
    if connection.write_all(response.as_bytes()).is_err() {
        sys::record_error("the connection closed during the handshake");
        return -1;
    }
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let socket = WebSocket {
        writer: FiberLock::new(Writer { socket: connection.clone(), close_sent: false }),
        reader: FiberLock::new(Reader {
            socket: connection,
            buffer: leftover,
            closed: false,
            last_heard: Instant::now(),
            ping_sent: false,
            deadline: None,
        }),
        query: request.query().to_string(),
        headers: request.headers_owned(),
        remote: request.remote(),
    };
    lock(&SOCKETS).insert(id, Arc::new(socket));
    request.set_upgraded(id);
    id
}

// ---- messages -------------------------------------------------------------------------

/// Wait for the next message. Returns 1 for text or 2 for binary, with the data available from
/// `lrt_ws_payload`; 0 once the connection is closed.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_ws_recv(id: i64) -> i64 {
    let Some(socket) = socket(id) else { return RECV_CLOSED };
    let mut reader = socket.reader.lock();
    if reader.closed {
        return RECV_CLOSED;
    }
    match reader.message(&socket) {
        Ok((opcode, data)) => {
            PAYLOAD.with(|p| *p.borrow_mut() = data);
            if opcode == OP_TEXT { RECV_TEXT } else { RECV_BINARY }
        }
        Err(stop) => {
            reader.closed = true;
            let mut writer = socket.writer.lock();
            match stop {
                Stop::Fail(code) => writer.close(code, b""),
                Stop::Gone(Some(code)) => writer.close(code, b""),
                Stop::Gone(None) => {}
            }
            RECV_CLOSED
        }
    }
}

/// The data of the message the last `lrt_ws_recv` on this thread returned.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_ws_payload() -> Str {
    PAYLOAD.with(|p| Str::from_bytes(&std::mem::take(&mut *p.borrow_mut())))
}

/// Send a text (`binary` = 0) or binary message. Returns 0, or -1 if the connection is closed.
///
/// # Safety
/// `ptr` and `len` describe the message's bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_ws_send(id: i64, binary: i64, ptr: *const u8, len: i64) -> i64 {
    let Some(socket) = socket(id) else { return -1 };
    let data = unsafe { Str::from_raw(ptr, len) }.as_bytes();
    let opcode = if binary != 0 { OP_BINARY } else { OP_TEXT };
    if socket.writer.lock().frame(opcode, data) { 0 } else { -1 }
}

/// Start the closing handshake with a status code and a reason.
///
/// # Safety
/// `ptr` and `len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_ws_close(id: i64, code: i64, ptr: *const u8, len: i64) {
    let Some(socket) = socket(id) else { return };
    let reason = unsafe { Str::from_raw(ptr, len) };
    let code = if (1000..=4999).contains(&code) { code as u16 } else { CLOSE_NORMAL };
    socket.writer.lock().close(code, reason.as_bytes());
}

/// After the handler returns: close the socket if it is still open, wait briefly for the peer's
/// close frame, and forget the socket.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_ws_finish(id: i64) {
    let Some(socket) = socket(id) else { return };
    {
        let mut reader = socket.reader.lock();
        socket.writer.lock().close(CLOSE_NORMAL, b"");
        reader.deadline = Some(Instant::now() + CLOSE_WAIT);
        // Discard anything else until the peer's close frame arrives (or it goes away).
        while !reader.closed {
            match reader.frame(&socket, usize::MAX >> 1) {
                Ok(frame) if frame.opcode == OP_CLOSE => reader.closed = true,
                Ok(_) => {}
                Err(_) => reader.closed = true,
            }
        }
    }
    lock(&SOCKETS).remove(&id);
}

/// Forget a socket whose handler did not call `lrt_ws_finish` (the HTTP layer calls this).
pub(crate) fn forget(id: i64) {
    lock(&SOCKETS).remove(&id);
}

/// Set the largest message accepted, in bytes, and the keep-alive ping interval in seconds.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_ws_set_limits(max_message: i64, ping_seconds: i64) {
    if max_message > 0 {
        MAX_MESSAGE.store(max_message, Ordering::Relaxed);
    }
    if ping_seconds > 0 {
        PING_SECONDS.store(ping_seconds, Ordering::Relaxed);
    }
}

// ---- the upgrade request, after the upgrade -------------------------------------------

/// A query parameter of the upgrade request, or `None`.
///
/// # Safety
/// `ptr` and `len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_ws_query(id: i64, ptr: *const u8, len: i64) -> Str {
    let wanted = unsafe { Str::from_raw(ptr, len) };
    let Some(socket) = socket(id) else { return http::optional(None) };
    http::optional(http::query_value(&socket.query, wanted).as_deref())
}

/// A header of the upgrade request, or `None`.
///
/// # Safety
/// `ptr` and `len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_ws_header(id: i64, ptr: *const u8, len: i64) -> Str {
    let wanted = unsafe { Str::from_raw(ptr, len) }.to_ascii_lowercase();
    let Some(socket) = socket(id) else { return http::optional(None) };
    http::optional(socket.headers.iter().find(|(n, _)| *n == wanted).map(|(_, v)| v.as_str()))
}

/// The peer's address.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_ws_remote(id: i64) -> Str {
    match socket(id) {
        Some(socket) => Str::from_bytes(socket.remote.as_bytes()),
        None => Str::empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rfc_example_handshake() {
        // RFC 6455, section 1.3.
        assert_eq!(accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
        assert_eq!(base64_len("dGhlIHNhbXBsZSBub25jZQ=="), Some(16));
        assert_eq!(base64_len("not base64!"), None);
    }

    #[test]
    fn sha1_matches_known_digests() {
        let hex = |d: [u8; 20]| d.iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(hex(sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(hex(sha1(b"abc")), "a9993e364706816aba3e25717850c26c9cd0d89d");
        let long = vec![b'a'; 1000];
        assert_eq!(hex(sha1(&long)), "291e9a6c66994949b57ba5e650361e98fc36b1ba");
    }

    #[test]
    fn close_codes() {
        assert_eq!(close_reply(&[]), Ok(1000));
        assert_eq!(close_reply(&[3]), Err(1002));
        assert_eq!(close_reply(&1001u16.to_be_bytes()), Ok(1001));
        assert_eq!(close_reply(&1005u16.to_be_bytes()), Err(1002));
        assert_eq!(close_reply(&4000u16.to_be_bytes()), Ok(4000));
        assert_eq!(close_reply(&[0x03, 0xE8, 0xFF]), Err(1007));
    }
}
