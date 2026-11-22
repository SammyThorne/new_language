//! A Redis client (docs/14, milestone 25): RESP3 over the same non-blocking socket as the HTTP
//! server, so a task waiting for Redis parks instead of blocking a worker.
//!
//! - `redis://[user[:password]]@host[:port][/db]`, and `rediss://` for TLS (the certificate is
//!   checked against the trusted roots and `SSL_CERT_FILE`). Options after `?`: `pool_max`
//!   (default 10) and `timeout` in milliseconds (default 30000) for one command's reply.
//! - A connection starts with `HELLO 3` (with `AUTH` when the URL has credentials); a server
//!   without it (Redis before 6) is spoken to in RESP2, with `AUTH` and `SELECT`. Both come out
//!   as the same replies.
//! - A pool per `open` call. A task that finds every connection busy waits for one (at most 30
//!   seconds). A connection that fails, or that a cancelled task was waiting on, is closed.
//! - Commands are sent as arrays of bulk strings, and any number of them in one write (a
//!   pipeline), optionally inside `MULTI`/`EXEC`. Replies are kept in a table, as a flat list of
//!   nodes, until the standard library has copied them out.
//! - A subscription takes a connection of its own out of the pool until it ends.
//!
//! Not supported: client-side caching, cluster and sentinel, streamed aggregates, RESP3 push
//! messages outside a subscription.

use crate::fiber::{self, WaitList, WaitNode};
use crate::http::Socket;
use crate::tls::{Protocol, Session};
use crate::{Str, sys};
use std::cell::RefCell;
use std::net::ToSocketAddrs;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const POOL_WAIT: Duration = Duration::from_secs(30);
const DEFAULT_TIMEOUT_MS: u64 = 30_000;
/// The longest bulk string Redis itself allows.
const MAX_BULK: usize = 512 * 1024 * 1024;
const MAX_ITEMS: usize = 1 << 26;
const MAX_DEPTH: usize = 64;

// ---- configuration ----------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
struct Config {
    host: String,
    port: u16,
    username: Option<String>,
    password: Option<String>,
    database: u32,
    tls: bool,
    pool_max: usize,
    timeout_ms: u64,
}

fn percent_decode(text: &str) -> Result<String, String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = text.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok());
            out.push(hex.ok_or_else(|| format!("a bad percent escape in `{text}`"))?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| format!("`{text}` is not UTF-8 once decoded"))
}

/// `redis://user:password@host:port/db?pool_max=...&timeout=...`
fn parse_url(url: &str) -> Result<Config, String> {
    let (tls, rest) = if let Some(rest) = url.strip_prefix("rediss://") {
        (true, rest)
    } else if let Some(rest) = url.strip_prefix("redis://") {
        (false, rest)
    } else {
        return Err("a Redis URL starts with `redis://` or `rediss://`".to_string());
    };
    let (rest, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (authority, database) = rest.split_once('/').unwrap_or((rest, ""));
    let (credentials, host_port) = match authority.rsplit_once('@') {
        Some((credentials, host_port)) => (Some(credentials), host_port),
        None => (None, authority),
    };
    // `redis://:secret@host` and `redis://secret@host` both mean a password with no user name,
    // as in the other clients; `user:secret` names the user.
    let (username, password) = match credentials {
        Some(c) => match c.split_once(':') {
            Some((user, password)) => {
                let user = percent_decode(user)?;
                ((!user.is_empty()).then_some(user), Some(percent_decode(password)?))
            }
            None => (None, Some(percent_decode(c)?)),
        },
        None => (None, None),
    };
    let (host, port) = if let Some(v6) = host_port.strip_prefix('[') {
        let (host, after) = v6.split_once(']').ok_or("an unclosed `[` in the host")?;
        (host.to_string(), after.strip_prefix(':'))
    } else {
        match host_port.rsplit_once(':') {
            Some((host, port)) => (host.to_string(), Some(port)),
            None => (host_port.to_string(), None),
        }
    };
    let port = match port {
        Some(p) => p.parse::<u16>().map_err(|_| format!("`{p}` is not a port number"))?,
        None => 6379,
    };
    let database = if database.is_empty() {
        0
    } else {
        database.parse::<u32>().map_err(|_| format!("`{database}` is not a database number"))?
    };
    let mut config = Config {
        host: if host.is_empty() { "localhost".to_string() } else { host },
        port,
        username,
        password,
        database,
        tls,
        pool_max: 10,
        timeout_ms: DEFAULT_TIMEOUT_MS,
    };
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        match name {
            "pool_max" => {
                config.pool_max = value
                    .parse::<usize>()
                    .ok()
                    .filter(|n| (1..=10_000).contains(n))
                    .ok_or_else(|| format!("`pool_max={value}` must be a number from 1 to 10000"))?;
            }
            "timeout" => {
                config.timeout_ms = value
                    .parse::<u64>()
                    .ok()
                    .filter(|n| (1..=86_400_000).contains(n))
                    .ok_or_else(|| format!("`timeout={value}` must be milliseconds, from 1 to 86400000"))?;
            }
            other => return Err(format!("unknown Redis URL option `{other}`")),
        }
    }
    Ok(config)
}

// ---- errors -----------------------------------------------------------------------------

/// Why an operation failed. The connection is closed after it.
#[derive(Debug)]
struct Error(String);

impl Error {
    fn broken(context: &str, error: impl std::fmt::Display) -> Error {
        Error(format!("{context}: {error}"))
    }
}

thread_local! {
    static ARGS: RefCell<Vec<Vec<u8>>> = const { RefCell::new(Vec::new()) };
    static COMMANDS: RefCell<Vec<Vec<Vec<u8>>>> = const { RefCell::new(Vec::new()) };
}

fn record(error: Error) {
    sys::record_error(error.0);
}

// ---- replies ----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Reply {
    Null,
    Status(Vec<u8>),
    Error(Vec<u8>),
    Int(i64),
    Text(Vec<u8>),
    Double(Vec<u8>),
    Bool(bool),
    List(Vec<Reply>),
    Map(Vec<(Reply, Reply)>),
}

/// How much of a reply is there, or what is missing.
enum Parsed {
    Done(Reply, usize),
    /// More bytes are needed.
    Incomplete,
}

fn line_end(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\r\n")
}

fn parse_int(text: &[u8]) -> Result<i64, Error> {
    std::str::from_utf8(text)
        .ok()
        .and_then(|t| t.parse::<i64>().ok())
        .ok_or_else(|| Error(format!("a bad number in a reply: {:?}", String::from_utf8_lossy(text))))
}

/// One reply from the start of `buf`, without consuming anything.
fn parse(buf: &[u8], depth: usize) -> Result<Parsed, Error> {
    if depth > MAX_DEPTH {
        return Err(Error("a reply nested too deeply".into()));
    }
    let Some(&kind) = buf.first() else {
        return Ok(Parsed::Incomplete);
    };
    let Some(end) = line_end(&buf[1..]) else {
        return Ok(Parsed::Incomplete);
    };
    let head = &buf[1..1 + end];
    let mut used = 1 + end + 2;
    let reply = match kind {
        b'+' => Reply::Status(head.to_vec()),
        b'-' => Reply::Error(head.to_vec()),
        b':' => Reply::Int(parse_int(head)?),
        b'_' => Reply::Null,
        b'#' => Reply::Bool(head == b"t"),
        b',' => Reply::Double(head.to_vec()),
        b'(' => Reply::Text(head.to_vec()),
        b'$' | b'!' | b'=' => {
            let length = parse_int(head)?;
            if length < 0 {
                return Ok(Parsed::Done(Reply::Null, used));
            }
            let length = usize::try_from(length).ok().filter(|&l| l <= MAX_BULK);
            let length = length.ok_or_else(|| Error("a bulk string too long".into()))?;
            if buf.len() < used + length + 2 {
                return Ok(Parsed::Incomplete);
            }
            let body = &buf[used..used + length];
            if &buf[used + length..used + length + 2] != b"\r\n" {
                return Err(Error("a bulk string without its end".into()));
            }
            used += length + 2;
            match kind {
                b'!' => Reply::Error(body.to_vec()),
                // A verbatim string starts with a three-letter format and a colon.
                b'=' if body.len() >= 4 && body[3] == b':' => Reply::Text(body[4..].to_vec()),
                _ => Reply::Text(body.to_vec()),
            }
        }
        b'*' | b'~' | b'>' => {
            let count = parse_int(head)?;
            if count < 0 {
                return Ok(Parsed::Done(Reply::Null, used));
            }
            let count = usize::try_from(count).ok().filter(|&c| c <= MAX_ITEMS);
            let count = count.ok_or_else(|| Error("an array too long".into()))?;
            let mut items = Vec::with_capacity(count.min(1024));
            for _ in 0..count {
                match parse(&buf[used..], depth + 1)? {
                    Parsed::Done(item, size) => {
                        items.push(item);
                        used += size;
                    }
                    Parsed::Incomplete => return Ok(Parsed::Incomplete),
                }
            }
            Reply::List(items)
        }
        b'%' => {
            let count = parse_int(head)?;
            let count = usize::try_from(count).ok().filter(|&c| c <= MAX_ITEMS);
            let count = count.ok_or_else(|| Error("a map too long".into()))?;
            let mut pairs = Vec::with_capacity(count.min(1024));
            for _ in 0..count {
                let key = match parse(&buf[used..], depth + 1)? {
                    Parsed::Done(item, size) => {
                        used += size;
                        item
                    }
                    Parsed::Incomplete => return Ok(Parsed::Incomplete),
                };
                let value = match parse(&buf[used..], depth + 1)? {
                    Parsed::Done(item, size) => {
                        used += size;
                        item
                    }
                    Parsed::Incomplete => return Ok(Parsed::Incomplete),
                };
                pairs.push((key, value));
            }
            Reply::Map(pairs)
        }
        b'|' => {
            // Attributes describe the reply that follows them; they are not part of it.
            let count = parse_int(head)?;
            let count = usize::try_from(count).ok().filter(|&c| c <= MAX_ITEMS);
            let count = count.ok_or_else(|| Error("an attribute map too long".into()))?;
            for _ in 0..count * 2 {
                match parse(&buf[used..], depth + 1)? {
                    Parsed::Done(_, size) => used += size,
                    Parsed::Incomplete => return Ok(Parsed::Incomplete),
                }
            }
            return match parse(&buf[used..], depth + 1)? {
                Parsed::Done(reply, size) => Ok(Parsed::Done(reply, used + size)),
                Parsed::Incomplete => Ok(Parsed::Incomplete),
            };
        }
        other => return Err(Error(format!("an unknown reply type {:?}", other as char))),
    };
    Ok(Parsed::Done(reply, used))
}

/// A reply as a flat list of nodes, for compiled code to walk.
mod kinds {
    pub const NULL: u8 = 0;
    pub const STATUS: u8 = 1;
    pub const ERROR: u8 = 2;
    pub const INT: u8 = 3;
    pub const TEXT: u8 = 4;
    pub const LIST: u8 = 5;
    pub const MAP: u8 = 6;
    pub const BOOL: u8 = 7;
    pub const DOUBLE: u8 = 8;
}

#[derive(Default)]
struct Node {
    kind: u8,
    int: i64,
    data: Vec<u8>,
    children: Vec<u32>,
}

#[derive(Default)]
struct Flat(Vec<Node>);

impl Flat {
    fn add(&mut self, reply: &Reply) -> u32 {
        let index = self.0.len() as u32;
        self.0.push(Node::default());
        let node = match reply {
            Reply::Null => Node { kind: kinds::NULL, ..Node::default() },
            Reply::Status(s) => Node { kind: kinds::STATUS, data: s.clone(), ..Node::default() },
            Reply::Error(s) => Node { kind: kinds::ERROR, data: s.clone(), ..Node::default() },
            Reply::Int(n) => Node { kind: kinds::INT, int: *n, ..Node::default() },
            Reply::Text(s) => Node { kind: kinds::TEXT, data: s.clone(), ..Node::default() },
            Reply::Double(s) => Node { kind: kinds::DOUBLE, data: s.clone(), ..Node::default() },
            Reply::Bool(b) => Node { kind: kinds::BOOL, int: i64::from(*b), ..Node::default() },
            Reply::List(items) => {
                let children = items.iter().map(|item| self.add(item)).collect();
                Node { kind: kinds::LIST, children, ..Node::default() }
            }
            Reply::Map(pairs) => {
                let mut children = Vec::with_capacity(pairs.len() * 2);
                for (key, value) in pairs {
                    children.push(self.add(key));
                    children.push(self.add(value));
                }
                Node { kind: kinds::MAP, children, ..Node::default() }
            }
        };
        self.0[index as usize] = node;
        index
    }
}

static RESULTS: Mutex<Slab<Flat>> = Mutex::new(Slab::new());

struct Slab<T> {
    items: Vec<Option<T>>,
    free: Vec<usize>,
}

impl<T> Slab<T> {
    const fn new() -> Slab<T> {
        Slab { items: Vec::new(), free: Vec::new() }
    }

    fn insert(&mut self, item: T) -> i64 {
        match self.free.pop() {
            Some(i) => {
                self.items[i] = Some(item);
                i as i64
            }
            None => {
                self.items.push(Some(item));
                self.items.len() as i64 - 1
            }
        }
    }

    fn get(&self, id: i64) -> Option<&T> {
        self.items.get(usize::try_from(id).ok()?)?.as_ref()
    }

    fn remove(&mut self, id: i64) -> Option<T> {
        let item = self.items.get_mut(usize::try_from(id).ok()?)?.take();
        if item.is_some() {
            self.free.push(id as usize);
        }
        item
    }
}

// ---- commands ---------------------------------------------------------------------------

fn encode(out: &mut Vec<u8>, command: &[Vec<u8>]) {
    out.extend_from_slice(format!("*{}\r\n", command.len()).as_bytes());
    for argument in command {
        out.extend_from_slice(format!("${}\r\n", argument.len()).as_bytes());
        out.extend_from_slice(argument);
        out.extend_from_slice(b"\r\n");
    }
}

fn words(items: &[&str]) -> Vec<Vec<u8>> {
    items.iter().map(|w| w.as_bytes().to_vec()).collect()
}

// ---- a connection -----------------------------------------------------------------------

struct Conn {
    socket: Socket,
    buffer: Vec<u8>,
    /// Where the unread bytes in `buffer` start.
    start: usize,
}

impl Conn {
    fn send(&self, bytes: &[u8]) -> Result<(), Error> {
        self.socket.write_all(bytes).map_err(|e| Error::broken("sending to Redis failed", e))
    }

    /// Read some more bytes into the buffer. `Ok(false)` when the deadline came first.
    fn more(&mut self, deadline: Instant) -> Result<bool, Error> {
        if self.start > 0 && self.start * 2 >= self.buffer.len() {
            self.buffer.drain(..self.start);
            self.start = 0;
        }
        let mut chunk = vec![0u8; 65536];
        match self.socket.read(&mut chunk, Some(deadline), false) {
            Ok(0) => Err(Error("Redis closed the connection".into())),
            Ok(k) => {
                self.buffer.extend_from_slice(&chunk[..k]);
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Ok(false),
            Err(e) => {
                if let Some(reason) = crate::cancel::reason_of(&e) {
                    return Err(Error(reason.message().to_string()));
                }
                Err(Error::broken("reading from Redis failed", e))
            }
        }
    }

    /// The next reply. `Ok(None)` if `deadline` passes first; what was read is kept for the next
    /// call.
    fn reply(&mut self, deadline: Instant) -> Result<Option<Reply>, Error> {
        loop {
            match parse(&self.buffer[self.start..], 0)? {
                Parsed::Done(reply, used) => {
                    self.start += used;
                    if self.start == self.buffer.len() {
                        self.buffer.clear();
                        self.start = 0;
                    }
                    return Ok(Some(reply));
                }
                Parsed::Incomplete => {
                    if !self.more(deadline)? {
                        return Ok(None);
                    }
                }
            }
        }
    }

    /// The next reply that is a command's answer; a late timeout is a broken connection.
    fn answer(&mut self, deadline: Instant) -> Result<Reply, Error> {
        self.reply(deadline)?.ok_or_else(|| Error("timed out waiting for Redis".into()))
    }

    fn open(config: &Config) -> Result<Conn, Error> {
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        let addresses = (config.host.as_str(), config.port)
            .to_socket_addrs()
            .map_err(|e| Error(format!("cannot resolve `{}`: {e}", config.host)))?;
        let mut failure = Error(format!("`{}` has no address", config.host));
        let mut socket = None;
        for address in addresses {
            match Socket::connect(&address, deadline) {
                Ok(s) => {
                    socket = Some(s);
                    break;
                }
                Err(e) => failure = Error(format!("cannot connect to Redis at {address}: {e}")),
            }
        }
        let mut socket = socket.ok_or(failure)?;
        if config.tls {
            let session = Session::client(&config.host, Protocol::Redis).map_err(Error)?;
            socket.start_tls(session);
        }
        let mut conn = Conn { socket, buffer: Vec::new(), start: 0 };
        conn.handshake(config, deadline)?;
        Ok(conn)
    }

    fn handshake(&mut self, config: &Config, deadline: Instant) -> Result<(), Error> {
        let mut hello = words(&["HELLO", "3"]);
        if let Some(password) = &config.password {
            hello.push(b"AUTH".to_vec());
            hello.push(config.username.clone().unwrap_or_else(|| "default".to_string()).into_bytes());
            hello.push(password.clone().into_bytes());
        }
        let mut out = Vec::new();
        encode(&mut out, &hello);
        self.send(&out)?;
        if let Reply::Error(message) = self.answer(deadline)? {
            let message = String::from_utf8_lossy(&message).into_owned();
            // A server before Redis 6 has no `HELLO`; others refuse the credentials.
            let old = message.contains("unknown command") || message.contains("NOPROTO");
            if !old {
                return Err(Error(format!("Redis refused the connection: {message}")));
            }
            if let Some(password) = &config.password {
                let mut auth = words(&["AUTH"]);
                if let Some(user) = &config.username {
                    auth.push(user.clone().into_bytes());
                }
                auth.push(password.clone().into_bytes());
                self.expect_ok(&auth, deadline)?;
            }
        }
        if config.database != 0 {
            self.expect_ok(&words(&["SELECT", &config.database.to_string()]), deadline)?;
        }
        Ok(())
    }

    fn expect_ok(&mut self, command: &[Vec<u8>], deadline: Instant) -> Result<(), Error> {
        let mut out = Vec::new();
        encode(&mut out, command);
        self.send(&out)?;
        match self.answer(deadline)? {
            Reply::Error(message) => {
                Err(Error(format!("Redis refused the connection: {}", String::from_utf8_lossy(&message))))
            }
            _ => Ok(()),
        }
    }

    /// Send commands in one write and read their replies. With `atomic`, they are wrapped in
    /// `MULTI` and `EXEC`, and the one reply is `EXEC`'s.
    fn run(&mut self, commands: &[Vec<Vec<u8>>], atomic: bool, timeout: Duration) -> Result<Vec<Reply>, Error> {
        let mut out = Vec::new();
        if atomic {
            encode(&mut out, &words(&["MULTI"]));
        }
        for command in commands {
            encode(&mut out, command);
        }
        if atomic {
            encode(&mut out, &words(&["EXEC"]));
        }
        self.send(&out)?;
        let deadline = Instant::now() + timeout;
        let mut replies = Vec::with_capacity(commands.len());
        if atomic {
            // MULTI's OK, then QUEUED (or an error, which EXEC then refuses) for each command.
            for _ in 0..commands.len() + 1 {
                self.answer(deadline)?;
            }
            replies.push(self.answer(deadline)?);
        } else {
            for _ in commands {
                replies.push(self.answer(deadline)?);
            }
        }
        Ok(replies)
    }

    fn terminate(&self) {
        self.socket.shutdown();
    }
}

// ---- the pool ---------------------------------------------------------------------------

struct PoolState {
    idle: Vec<Conn>,
    /// Connections open, idle or in use.
    open: usize,
    waiters: WaitList,
}

struct Pool {
    config: Config,
    state: Mutex<PoolState>,
}

// SAFETY: the wait list's nodes are only reached under the pool's lock (see `fiber::WaitList`).
unsafe impl Send for Pool {}
unsafe impl Sync for Pool {}

static POOLS: Mutex<Vec<Arc<Pool>>> = Mutex::new(Vec::new());

impl Pool {
    fn acquire(&self) -> Result<Conn, Error> {
        let deadline = Instant::now() + POOL_WAIT;
        loop {
            let node = WaitNode::new();
            {
                let mut state = fiber::lock(&self.state);
                if let Some(conn) = state.idle.pop() {
                    return Ok(conn);
                }
                if state.open < self.config.pool_max {
                    state.open += 1;
                    drop(state);
                    return Conn::open(&self.config).inspect_err(|_| self.closed());
                }
                state.waiters.push(&node);
            }
            while !node.signaled() && Instant::now() < deadline && crate::cancel::check().is_none() {
                fiber::park(Some(deadline));
            }
            if !node.signaled() {
                let mut state = fiber::lock(&self.state);
                state.waiters.remove(&node);
                drop(state);
                if let Some(reason) = crate::cancel::check()
                    && !node.signaled()
                {
                    return Err(Error(reason.message().to_string()));
                }
                if !node.signaled() {
                    return Err(Error(format!(
                        "every Redis connection (pool_max={}) stayed busy for {} seconds",
                        self.config.pool_max,
                        POOL_WAIT.as_secs()
                    )));
                }
            }
        }
    }

    fn release(&self, conn: Conn) {
        let mut state = fiber::lock(&self.state);
        state.idle.push(conn);
        state.waiters.wake_one();
    }

    /// A connection has gone: let a waiter open another.
    fn closed(&self) {
        let mut state = fiber::lock(&self.state);
        state.open -= 1;
        state.waiters.wake_one();
    }

    /// Run `f` on a connection from the pool, closing it if it broke.
    fn with<T>(&self, f: impl FnOnce(&mut Conn) -> Result<T, Error>) -> Result<T, Error> {
        let mut conn = self.acquire()?;
        let result = f(&mut conn);
        if result.is_err() {
            conn.terminate();
            self.closed();
        } else {
            self.release(conn);
        }
        result
    }
}

fn pool(id: i64) -> Option<Arc<Pool>> {
    fiber::lock(&POOLS).get(usize::try_from(id).ok()?).cloned()
}

/// A subscription: a connection out of its pool, in subscriber mode.
struct Subscription {
    pool: Arc<Pool>,
    conn: Conn,
}

/// Its place in the table. The connection is taken out while a task waits on it, so that no lock
/// is held across the wait; `ended` says that `unsubscribe` came meanwhile.
struct Slot {
    inner: Mutex<Option<Subscription>>,
    ended: std::sync::atomic::AtomicBool,
}

static SUBSCRIPTIONS: Mutex<Slab<Arc<Slot>>> = Mutex::new(Slab::new());

fn end(subscription: Subscription) {
    subscription.conn.terminate();
    subscription.pool.closed();
}

fn take_commands() -> Vec<Vec<Vec<u8>>> {
    let pending = ARGS.with(|a| std::mem::take(&mut *a.borrow_mut()));
    COMMANDS.with(|c| {
        let mut commands = c.borrow_mut();
        if !pending.is_empty() {
            commands.push(pending);
        }
        std::mem::take(&mut *commands)
    })
}

fn store(replies: Vec<Reply>) -> i64 {
    let mut flat = Flat::default();
    // Node 0 is the list of replies, one a command.
    flat.0.push(Node { kind: kinds::LIST, ..Node::default() });
    let children: Vec<u32> = replies.iter().map(|reply| flat.add(reply)).collect();
    flat.0[0].children = children;
    fiber::lock(&RESULTS).insert(flat)
}

// ---- the functions compiled code calls --------------------------------------------------

/// Bytes, not necessarily UTF-8: Redis values are binary.
unsafe fn bytes_of<'a>(ptr: *const u8, len: i64) -> &'a [u8] {
    if len <= 0 {
        return &[];
    }
    // SAFETY: guaranteed by the caller.
    unsafe { std::slice::from_raw_parts(ptr, len as usize) }
}

/// Open a pool for a `redis://` URL, checking it with one connection. Returns the pool's id, or
/// -1 after recording the error.
///
/// # Safety
/// `url` and `url_len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_redis_open(url: *const u8, url_len: i64) -> i64 {
    let url = unsafe { Str::from_raw(url, url_len) };
    let config = match parse_url(url) {
        Ok(config) => config,
        Err(message) => {
            record(Error(message));
            return -1;
        }
    };
    let pool =
        Arc::new(Pool { config, state: Mutex::new(PoolState { idle: Vec::new(), open: 0, waiters: WaitList::new() }) });
    match pool.acquire() {
        Ok(conn) => pool.release(conn),
        Err(error) => {
            record(error);
            return -1;
        }
    }
    let mut pools = fiber::lock(&POOLS);
    pools.push(pool);
    pools.len() as i64 - 1
}

/// Add an argument to the command being built on this thread.
///
/// # Safety
/// `ptr` and `len` describe bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_redis_arg(ptr: *const u8, len: i64) {
    let value = unsafe { bytes_of(ptr, len) }.to_vec();
    ARGS.with(|a| a.borrow_mut().push(value));
}

/// Finish the command being built, and start another.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_redis_end_command() {
    let command = ARGS.with(|a| std::mem::take(&mut *a.borrow_mut()));
    if !command.is_empty() {
        COMMANDS.with(|c| c.borrow_mut().push(command));
    }
}

/// Send the commands built on this thread, in one write, on a connection of the pool, and read
/// their replies. `atomic` wraps them in `MULTI` and `EXEC` and answers with `EXEC`'s reply.
/// `timeout_ms` bounds the wait for the replies; 0 takes the pool's. Returns a result handle (node
/// 0 is the list of replies), or -1 after recording the error.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_redis_run(pool_id: i64, atomic: i64, timeout_ms: i64) -> i64 {
    let commands = take_commands();
    let Some(pool) = pool(pool_id) else {
        record(Error("not a Redis connection".into()));
        return -1;
    };
    if commands.is_empty() {
        record(Error("no command to run".into()));
        return -1;
    }
    let timeout = if timeout_ms > 0 { timeout_ms as u64 } else { pool.config.timeout_ms };
    match pool.with(|conn| conn.run(&commands, atomic != 0, Duration::from_millis(timeout))) {
        Ok(replies) => store(replies),
        Err(error) => {
            record(error);
            -1
        }
    }
}

fn with_node<T>(handle: i64, node: i64, default: T, f: impl FnOnce(&Node) -> T) -> T {
    let results = fiber::lock(&RESULTS);
    results.get(handle).and_then(|flat| flat.0.get(usize::try_from(node).ok()?)).map_or(default, f)
}

/// What a node is: 0 null, 1 status, 2 error, 3 integer, 4 text, 5 list, 6 map (keys and values
/// alternate among its children), 7 boolean, 8 double (as text).
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_redis_kind(handle: i64, node: i64) -> i64 {
    with_node(handle, node, 0, |n| i64::from(n.kind))
}

/// An integer or boolean node's value.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_redis_int(handle: i64, node: i64) -> i64 {
    with_node(handle, node, 0, |n| n.int)
}

/// A status, error, text or double node's bytes.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_redis_text(handle: i64, node: i64) -> Str {
    with_node(handle, node, Str::empty(), |n| Str::from_bytes(&n.data))
}

/// How many children a list or map node has.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_redis_len(handle: i64, node: i64) -> i64 {
    with_node(handle, node, 0, |n| n.children.len() as i64)
}

/// The node number of a node's `index`th child.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_redis_child(handle: i64, node: i64, index: i64) -> i64 {
    with_node(handle, node, -1, |n| {
        usize::try_from(index).ok().and_then(|i| n.children.get(i)).map_or(-1, |&c| i64::from(c))
    })
}

/// Forget a result.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_redis_free(handle: i64) {
    fiber::lock(&RESULTS).remove(handle);
}

/// Subscribe to the channels (or, with `pattern`, the glob patterns) added as arguments since the
/// last command, on a connection of its own. Returns the subscription's id, or -1.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_redis_subscribe(pool_id: i64, pattern: i64) -> i64 {
    let channels = ARGS.with(|a| std::mem::take(&mut *a.borrow_mut()));
    COMMANDS.with(|c| c.borrow_mut().clear());
    let Some(pool) = pool(pool_id) else {
        record(Error("not a Redis connection".into()));
        return -1;
    };
    if channels.is_empty() {
        record(Error("subscribe to at least one channel".into()));
        return -1;
    }
    let mut conn = match pool.acquire() {
        Ok(conn) => conn,
        Err(error) => {
            record(error);
            return -1;
        }
    };
    let mut command = vec![if pattern != 0 { b"PSUBSCRIBE".to_vec() } else { b"SUBSCRIBE".to_vec() }];
    let wanted = channels.len();
    command.extend(channels);
    let mut out = Vec::new();
    encode(&mut out, &command);
    let deadline = Instant::now() + Duration::from_millis(pool.config.timeout_ms);
    let outcome = conn.send(&out).and_then(|()| {
        // One confirmation for each channel.
        for _ in 0..wanted {
            if let Reply::Error(message) = conn.answer(deadline)? {
                return Err(Error(format!("Redis refused to subscribe: {}", String::from_utf8_lossy(&message))));
            }
        }
        Ok(())
    });
    match outcome {
        Ok(()) => {
            let slot = Slot {
                inner: Mutex::new(Some(Subscription { pool, conn })),
                ended: std::sync::atomic::AtomicBool::new(false),
            };
            fiber::lock(&SUBSCRIPTIONS).insert(Arc::new(slot))
        }
        Err(error) => {
            conn.terminate();
            pool.closed();
            record(error);
            -1
        }
    }
}

/// The next message of a subscription, waiting at most `timeout_ms`: a result handle whose node 0
/// is the list of replies (one: `["message", channel, payload]`, or `["pmessage", pattern,
/// channel, payload]`), -2 if none came, or -1 after recording the error (the subscription is
/// over then).
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_redis_next(sub: i64, timeout_ms: i64) -> i64 {
    use std::sync::atomic::Ordering;
    let Some(slot) = fiber::lock(&SUBSCRIPTIONS).get(sub).cloned() else {
        record(Error("the subscription has ended".into()));
        return -1;
    };
    // The connection is taken out while it waits, so that no lock is held.
    let Some(mut subscription) = fiber::lock(&slot.inner).take() else {
        record(Error("the subscription is being read by another task, or has ended".into()));
        return -1;
    };
    let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(0) as u64);
    let mut result = -2;
    loop {
        match subscription.conn.reply(deadline) {
            Ok(Some(Reply::List(items))) => {
                let kind = match items.first() {
                    Some(Reply::Text(k)) | Some(Reply::Status(k)) => k.clone(),
                    _ => Vec::new(),
                };
                // Confirmations of more subscriptions and pongs are not messages.
                if kind == b"message" || kind == b"pmessage" {
                    result = store(vec![Reply::List(items)]);
                    break;
                }
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(error) => {
                end(subscription);
                fiber::lock(&SUBSCRIPTIONS).remove(sub);
                record(error);
                return -1;
            }
        }
    }
    if slot.ended.load(Ordering::SeqCst) {
        end(subscription);
        record(Error("the subscription has ended".into()));
        return -1;
    }
    *fiber::lock(&slot.inner) = Some(subscription);
    result
}

/// End a subscription: close its connection.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_redis_unsubscribe(sub: i64) {
    use std::sync::atomic::Ordering;
    if let Some(slot) = fiber::lock(&SUBSCRIPTIONS).remove(sub) {
        slot.ended.store(true, Ordering::SeqCst);
        if let Some(subscription) = fiber::lock(&slot.inner).take() {
            end(subscription);
        }
    }
}

/// Close the pool's idle connections. Connections in use close when they come back.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_redis_close(pool_id: i64) {
    if let Some(pool) = pool(pool_id) {
        let idle = std::mem::take(&mut fiber::lock(&pool.state).idle);
        for conn in idle {
            conn.terminate();
            pool.closed();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(bytes: &[u8]) -> Reply {
        match parse(bytes, 0).unwrap() {
            Parsed::Done(reply, used) => {
                assert_eq!(used, bytes.len(), "all of {:?} is the reply", String::from_utf8_lossy(bytes));
                reply
            }
            Parsed::Incomplete => panic!("incomplete: {:?}", String::from_utf8_lossy(bytes)),
        }
    }

    #[test]
    fn urls() {
        let c = parse_url("redis://ann:p%40ss@cache.example:6380/2?pool_max=3&timeout=500").unwrap();
        assert_eq!((c.username.as_deref(), c.password.as_deref()), (Some("ann"), Some("p@ss")));
        assert_eq!(
            (c.host.as_str(), c.port, c.database, c.tls, c.pool_max, c.timeout_ms),
            ("cache.example", 6380, 2, false, 3, 500)
        );
        let c = parse_url("rediss://:secret@[::1]/").unwrap();
        assert_eq!(
            (c.host.as_str(), c.port, c.username, c.password.as_deref(), c.tls),
            ("::1", 6379, None, Some("secret"), true)
        );
        let c = parse_url("redis://secret@localhost").unwrap();
        assert_eq!((c.username, c.password.as_deref()), (None, Some("secret")));
        assert!(parse_url("http://x").is_err());
        assert!(parse_url("redis://x/abc").unwrap_err().contains("abc"));
        assert!(parse_url("redis://x?mode=fast").unwrap_err().contains("mode"));
    }

    #[test]
    fn replies() {
        assert_eq!(one(b"+OK\r\n"), Reply::Status(b"OK".to_vec()));
        assert_eq!(one(b"-ERR nope\r\n"), Reply::Error(b"ERR nope".to_vec()));
        assert_eq!(one(b":-42\r\n"), Reply::Int(-42));
        assert_eq!(one(b"$5\r\nhe\r\no\r\n"), Reply::Text(b"he\r\no".to_vec()));
        assert_eq!(one(b"$-1\r\n"), Reply::Null);
        assert_eq!(one(b"_\r\n"), Reply::Null);
        assert_eq!(one(b"#t\r\n"), Reply::Bool(true));
        assert_eq!(one(b",1.5\r\n"), Reply::Double(b"1.5".to_vec()));
        assert_eq!(one(b"=7\r\ntxt:abc\r\n"), Reply::Text(b"abc".to_vec()));
        assert_eq!(one(b"!4\r\nBAD!\r\n"), Reply::Error(b"BAD!".to_vec()));
        assert_eq!(
            one(b"*2\r\n:1\r\n*1\r\n+x\r\n"),
            Reply::List(vec![Reply::Int(1), Reply::List(vec![Reply::Status(b"x".to_vec())])])
        );
        assert_eq!(one(b"%1\r\n+k\r\n:2\r\n"), Reply::Map(vec![(Reply::Status(b"k".to_vec()), Reply::Int(2))]));
        assert_eq!(
            one(b">3\r\n$7\r\nmessage\r\n$1\r\nc\r\n$1\r\np\r\n").clone(),
            Reply::List(vec![Reply::Text(b"message".to_vec()), Reply::Text(b"c".to_vec()), Reply::Text(b"p".to_vec())])
        );
        // Attributes are skipped.
        assert_eq!(one(b"|1\r\n+ttl\r\n:3\r\n:7\r\n"), Reply::Int(7));
    }

    #[test]
    fn partial_replies_are_incomplete_at_every_cut() {
        let whole = b"*3\r\n$3\r\nfoo\r\n:12\r\n%1\r\n+a\r\n$2\r\nbc\r\n";
        for cut in 0..whole.len() {
            assert!(matches!(parse(&whole[..cut], 0).unwrap(), Parsed::Incomplete), "cut at {cut}");
        }
        assert!(matches!(parse(whole, 0).unwrap(), Parsed::Done(_, used) if used == whole.len()));
        assert!(parse(b"?x\r\n", 0).is_err());
        assert!(parse(b"$999999999999\r\n", 0).is_err());
    }

    #[test]
    fn commands_and_flat_replies() {
        let mut out = Vec::new();
        encode(&mut out, &[b"SET".to_vec(), b"k".to_vec(), b"a b".to_vec()]);
        assert_eq!(out, b"*3\r\n$3\r\nSET\r\n$1\r\nk\r\n$3\r\na b\r\n");
        let mut flat = Flat::default();
        flat.add(&Reply::List(vec![Reply::Int(5), Reply::Text(b"x".to_vec())]));
        assert_eq!(flat.0.len(), 3);
        assert_eq!(flat.0[0].children, vec![1, 2]);
        assert_eq!((flat.0[1].kind, flat.0[1].int), (kinds::INT, 5));
        assert_eq!((flat.0[2].kind, flat.0[2].data.as_slice()), (kinds::TEXT, &b"x"[..]));
    }
}
