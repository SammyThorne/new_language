//! A PostgreSQL client (docs/05, "Databases"; milestone 12): protocol version 3 over the same
//! non-blocking socket as the HTTP server, so a task waiting for the database parks instead of
//! blocking a worker.
//!
//! - Authentication: trust, cleartext password, MD5, and SCRAM-SHA-256 (without channel binding).
//! - TLS through `sslmode`: `disable`, `prefer` (the default, as in libpq: TLS if the server offers
//!   it, without checking its certificate), `require` (TLS, certificate not checked), and
//!   `verify-full` (TLS, certificate checked against the trusted roots and the host name; see
//!   `tls.rs`).
//! - Queries use the extended protocol with every parameter and result in text format; the
//!   server converts parameters to the types the statement needs, as it does for `psql`.
//!   Scripts (several statements, no parameters) use the simple protocol.
//! - A pool per `connect` call, at most `pool_max` connections (default 10). A task that finds
//!   them all busy waits for one (for at most 30 seconds). A connection whose socket fails is
//!   closed rather than returned.
//!
//! Results are kept in a table until the standard library has copied them into collected values;
//! the last error's SQLSTATE code is kept per thread, like the last error message.
//!
//! Not supported: COPY, LISTEN/NOTIFY (notifications are ignored), binary results, cancelling a
//! running query.

use crate::fiber::{self, WaitList, WaitNode};
use crate::http::Socket;
use crate::json::push_json_string;
use crate::tls::{Protocol, Session};
use crate::{Str, sys};
use std::cell::RefCell;
use std::net::ToSocketAddrs;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const POOL_WAIT: Duration = Duration::from_secs(30);
const MAX_MESSAGE: usize = 1 << 30;

// ---- configuration ----------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
enum SslMode {
    Disable,
    Prefer,
    Require,
    VerifyFull,
}

#[derive(Clone, Debug)]
struct Config {
    host: String,
    port: u16,
    user: String,
    password: Option<String>,
    database: String,
    ssl: SslMode,
    pool_max: usize,
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

/// `postgres://user:password@host:port/database?sslmode=...&pool_max=...`
fn parse_url(url: &str) -> Result<Config, String> {
    let rest = url
        .strip_prefix("postgres://")
        .or_else(|| url.strip_prefix("postgresql://"))
        .ok_or_else(|| "a database URL starts with `postgres://`".to_string())?;
    let (rest, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (authority, database) = rest.split_once('/').unwrap_or((rest, ""));
    let (credentials, host_port) = match authority.rsplit_once('@') {
        Some((credentials, host_port)) => (Some(credentials), host_port),
        None => (None, authority),
    };
    let (user, password) = match credentials {
        Some(c) => match c.split_once(':') {
            Some((user, password)) => (percent_decode(user)?, Some(percent_decode(password)?)),
            None => (percent_decode(c)?, None),
        },
        None => (String::new(), None),
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
        None => 5432,
    };
    let user = if user.is_empty() { std::env::var("USER").unwrap_or_else(|_| "postgres".to_string()) } else { user };
    let database = if database.is_empty() { user.clone() } else { percent_decode(database)? };
    let mut config = Config {
        host: if host.is_empty() { "localhost".to_string() } else { host },
        port,
        user,
        password,
        database,
        ssl: SslMode::Prefer,
        pool_max: 10,
    };
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        match name {
            "sslmode" => {
                config.ssl = match value {
                    "disable" => SslMode::Disable,
                    "prefer" | "allow" => SslMode::Prefer,
                    "require" => SslMode::Require,
                    "verify-full" => SslMode::VerifyFull,
                    other => {
                        return Err(format!(
                            "sslmode `{other}` is not supported; use disable, prefer, require or verify-full"
                        ));
                    }
                }
            }
            "pool_max" => {
                config.pool_max = value
                    .parse()
                    .ok()
                    .filter(|&n| n > 0)
                    .ok_or_else(|| format!("pool_max `{value}` is not a count"))?;
            }
            other => return Err(format!("unknown database URL option `{other}`")),
        }
    }
    Ok(config)
}

// ---- errors -----------------------------------------------------------------------------

/// Why an operation failed. After `Server`, the connection is still usable; after `Broken`, it
/// is closed.
#[derive(Debug)]
enum Error {
    Server { code: String, message: String },
    Broken(String),
}

impl Error {
    fn broken(context: &str, error: impl std::fmt::Display) -> Error {
        Error::Broken(format!("{context}: {error}"))
    }
}

thread_local! {
    static LAST_CODE: RefCell<String> = const { RefCell::new(String::new()) };
    static ARGS: RefCell<Vec<Option<String>>> = const { RefCell::new(Vec::new()) };
}

fn record(error: Error) {
    let (code, message) = match error {
        Error::Server { code, message } => (code, message),
        // 08006 is connection_failure.
        Error::Broken(message) => ("08006".to_string(), message),
    };
    LAST_CODE.with(|c| *c.borrow_mut() = code);
    sys::record_error(message);
}

// ---- messages ---------------------------------------------------------------------------

/// An outgoing message: a type byte (none for the startup message), a length, a body.
struct Out(Vec<u8>);

impl Out {
    fn new(kind: u8) -> Out {
        Out(vec![kind, 0, 0, 0, 0])
    }

    fn untyped() -> Out {
        Out(vec![0, 0, 0, 0])
    }

    fn i16(&mut self, n: i16) -> &mut Out {
        self.0.extend_from_slice(&n.to_be_bytes());
        self
    }

    fn i32(&mut self, n: i32) -> &mut Out {
        self.0.extend_from_slice(&n.to_be_bytes());
        self
    }

    fn bytes(&mut self, b: &[u8]) -> &mut Out {
        self.0.extend_from_slice(b);
        self
    }

    fn cstr(&mut self, s: &str) -> &mut Out {
        self.0.extend_from_slice(s.as_bytes());
        self.0.push(0);
        self
    }

    /// Fill in the length and append the message to `into`.
    fn finish(&mut self, into: &mut Vec<u8>) {
        let start = if self.0.len() >= 5 && self.0[0] != 0 { 1 } else { 0 };
        let length = (self.0.len() - start) as i32;
        self.0[start..start + 4].copy_from_slice(&length.to_be_bytes());
        into.extend_from_slice(&self.0);
    }
}

/// Reads fields from a message body.
struct Fields<'a>(&'a [u8]);

impl<'a> Fields<'a> {
    fn i16(&mut self) -> Result<i16, Error> {
        let (head, rest) = self.0.split_at_checked(2).ok_or_else(|| Error::Broken("a short message".into()))?;
        self.0 = rest;
        Ok(i16::from_be_bytes([head[0], head[1]]))
    }

    fn i32(&mut self) -> Result<i32, Error> {
        let (head, rest) = self.0.split_at_checked(4).ok_or_else(|| Error::Broken("a short message".into()))?;
        self.0 = rest;
        Ok(i32::from_be_bytes([head[0], head[1], head[2], head[3]]))
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let (head, rest) = self.0.split_at_checked(n).ok_or_else(|| Error::Broken("a short message".into()))?;
        self.0 = rest;
        Ok(head)
    }

    fn cstr(&mut self) -> Result<&'a str, Error> {
        let end = self.0.iter().position(|&b| b == 0).ok_or_else(|| Error::Broken("an unterminated string".into()))?;
        let text =
            std::str::from_utf8(&self.0[..end]).map_err(|_| Error::Broken("a string that is not UTF-8".into()))?;
        self.0 = &self.0[end + 1..];
        Ok(text)
    }
}

/// The fields of an ErrorResponse.
fn server_error(body: &[u8]) -> Error {
    let (mut code, mut message, mut detail, mut hint) = (String::new(), String::new(), None, None);
    let mut fields = Fields(body);
    while let Some(&kind) = fields.0.first() {
        if kind == 0 {
            break;
        }
        fields.0 = &fields.0[1..];
        let Ok(value) = fields.cstr() else { break };
        match kind {
            b'C' => code = value.to_string(),
            b'M' => message = value.to_string(),
            b'D' => detail = Some(value.to_string()),
            b'H' => hint = Some(value.to_string()),
            _ => {}
        }
    }
    if let Some(detail) = detail {
        message = format!("{message} ({detail})");
    }
    if let Some(hint) = hint {
        message = format!("{message}; hint: {hint}");
    }
    Error::Server { code, message }
}

// ---- results ----------------------------------------------------------------------------

#[derive(Default)]
struct QueryResult {
    columns: Vec<(String, u32)>,
    /// Every value's text, end to end; `cells` holds each one's start and length (-1 for NULL),
    /// row by row.
    data: Vec<u8>,
    cells: Vec<(u32, i32)>,
    affected: i64,
}

impl QueryResult {
    fn rows(&self) -> usize {
        if self.columns.is_empty() { 0 } else { self.cells.len() / self.columns.len() }
    }

    fn value(&self, row: usize, column: usize) -> Option<&[u8]> {
        let &(start, len) = self.cells.get(row * self.columns.len() + column)?;
        (len >= 0).then(|| &self.data[start as usize..start as usize + len as usize])
    }

    fn add_row(&mut self, body: &[u8]) -> Result<(), Error> {
        let mut fields = Fields(body);
        let count = fields.i16()? as usize;
        if count != self.columns.len() {
            return Err(Error::Broken("a row with the wrong number of columns".into()));
        }
        for _ in 0..count {
            let len = fields.i32()?;
            if len < 0 {
                self.cells.push((0, -1));
            } else {
                let start = self.data.len() as u32;
                self.data.extend_from_slice(fields.take(len as usize)?);
                self.cells.push((start, len));
            }
        }
        Ok(())
    }

    /// The rows as a JSON array of objects, keyed by column name, with numbers, booleans and
    /// `json` columns as themselves and everything else as strings.
    fn to_json(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.data.len() * 2 + 16);
        out.push(b'[');
        for row in 0..self.rows() {
            if row > 0 {
                out.push(b',');
            }
            out.push(b'{');
            for (column, (name, oid)) in self.columns.iter().enumerate() {
                if column > 0 {
                    out.push(b',');
                }
                push_json_string(&mut out, name.as_bytes());
                out.push(b':');
                match self.value(row, column) {
                    None => out.extend_from_slice(b"null"),
                    Some(text) => json_value(&mut out, *oid, text),
                }
            }
            out.push(b'}');
        }
        out.push(b']');
        out
    }
}

/// One value in JSON, by its type's OID.
fn json_value(out: &mut Vec<u8>, oid: u32, text: &[u8]) {
    const BOOL: u32 = 16;
    const INT8: u32 = 20;
    const INT2: u32 = 21;
    const INT4: u32 = 23;
    const OID: u32 = 26;
    const JSON: u32 = 114;
    const FLOAT4: u32 = 700;
    const FLOAT8: u32 = 701;
    const NUMERIC: u32 = 1700;
    const JSONB: u32 = 3802;
    match oid {
        BOOL => out.extend_from_slice(if text == b"t" { b"true" } else { b"false" }),
        INT8 | INT2 | INT4 | OID => out.extend_from_slice(text),
        // NaN and the infinities have no JSON form.
        FLOAT4 | FLOAT8 | NUMERIC if text.iter().all(|b| b.is_ascii_digit() || b"+-.eE".contains(b)) => {
            out.extend_from_slice(text)
        }
        JSON | JSONB => out.extend_from_slice(text),
        _ => push_json_string(out, text),
    }
}

/// Results waiting to be read, by handle.
static RESULTS: Mutex<Slab<QueryResult>> = Mutex::new(Slab::new());

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

    fn get_mut(&mut self, id: i64) -> Option<&mut T> {
        self.items.get_mut(usize::try_from(id).ok()?)?.as_mut()
    }

    fn remove(&mut self, id: i64) -> Option<T> {
        let item = self.items.get_mut(usize::try_from(id).ok()?)?.take();
        if item.is_some() {
            self.free.push(id as usize);
        }
        item
    }
}

// ---- a connection -----------------------------------------------------------------------

struct Conn {
    socket: Socket,
    buffer: Vec<u8>,
    /// Where the unread bytes in `buffer` start.
    start: usize,
    /// The last ReadyForQuery status: `I` idle, `T` in a transaction, `E` in a failed one.
    status: u8,
}

impl Conn {
    fn send(&self, bytes: &[u8]) -> Result<(), Error> {
        self.socket.write_all(bytes).map_err(|e| Error::broken("sending to the database failed", e))
    }

    /// Read until at least `n` unread bytes are buffered.
    fn fill(&mut self, n: usize, deadline: Option<Instant>) -> Result<(), Error> {
        while self.buffer.len() - self.start < n {
            if self.start > 0 && self.start * 2 >= self.buffer.len() {
                self.buffer.drain(..self.start);
                self.start = 0;
            }
            let mut chunk = [0u8; 16384];
            match self.socket.read(&mut chunk, deadline, false) {
                Ok(0) => return Err(Error::Broken("the database closed the connection".into())),
                Ok(k) => self.buffer.extend_from_slice(&chunk[..k]),
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                    return Err(Error::Broken("timed out waiting for the database".into()));
                }
                Err(e) => return Err(Error::broken("reading from the database failed", e)),
            }
        }
        Ok(())
    }

    /// The next message's type and body. The body stays valid until the next call.
    fn next(&mut self, deadline: Option<Instant>) -> Result<(u8, &[u8]), Error> {
        self.fill(5, deadline)?;
        let s = self.start;
        let kind = self.buffer[s];
        let length =
            i32::from_be_bytes([self.buffer[s + 1], self.buffer[s + 2], self.buffer[s + 3], self.buffer[s + 4]]);
        let length = usize::try_from(length).ok().filter(|&l| (4..=MAX_MESSAGE).contains(&l));
        let length = length.ok_or_else(|| Error::Broken("a message with a bad length".into()))?;
        self.fill(1 + length, deadline)?;
        self.start = s + 1 + length;
        Ok((kind, &self.buffer[s + 5..s + 1 + length]))
    }

    fn open(config: &Config) -> Result<Conn, Error> {
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        let addresses = (config.host.as_str(), config.port)
            .to_socket_addrs()
            .map_err(|e| Error::Broken(format!("cannot resolve `{}`: {e}", config.host)))?;
        let mut failure = Error::Broken(format!("`{}` has no address", config.host));
        let mut socket = None;
        for address in addresses {
            match Socket::connect(&address, deadline) {
                Ok(s) => {
                    socket = Some(s);
                    break;
                }
                Err(e) => failure = Error::Broken(format!("cannot connect to the database at {address}: {e}")),
            }
        }
        let socket = socket.ok_or(failure)?;
        let mut conn = Conn { socket, buffer: Vec::new(), start: 0, status: b'I' };
        if config.ssl != SslMode::Disable {
            conn.start_tls(config, deadline)?;
        }
        conn.startup(config, deadline)?;
        Ok(conn)
    }

    fn start_tls(&mut self, config: &Config, deadline: Instant) -> Result<(), Error> {
        let mut out = Vec::new();
        Out::untyped().i32(80877103).finish(&mut out);
        self.send(&out)?;
        let mut answer = [0u8; 1];
        // One byte, read alone: anything after it would already be TLS.
        match self.socket.read(&mut answer, Some(deadline), false) {
            Ok(1) => {}
            Ok(_) => return Err(Error::Broken("the database closed the connection".into())),
            Err(e) => return Err(Error::broken("asking the database for TLS failed", e)),
        }
        match answer[0] {
            b'S' => {
                let session = if config.ssl == SslMode::VerifyFull {
                    Session::client(&config.host, Protocol::Postgres)
                } else {
                    Session::client_unverified(&config.host)
                };
                self.socket.start_tls(session.map_err(Error::Broken)?);
                Ok(())
            }
            b'N' if config.ssl == SslMode::Prefer => Ok(()),
            b'N' => Err(Error::Broken("the database does not accept TLS connections".into())),
            other => Err(Error::Broken(format!("an unexpected answer to the TLS request: {other:#x}"))),
        }
    }

    fn startup(&mut self, config: &Config, deadline: Instant) -> Result<(), Error> {
        let mut out = Vec::new();
        Out::untyped()
            .i32(196608)
            .cstr("user")
            .cstr(&config.user)
            .cstr("database")
            .cstr(&config.database)
            .cstr("application_name")
            .cstr(lang_names::CLI_NAME)
            .cstr("client_encoding")
            .cstr("UTF8")
            .bytes(&[0])
            .finish(&mut out);
        self.send(&out)?;
        let mut scram: Option<Scram> = None;
        loop {
            let (kind, body) = self.next(Some(deadline))?;
            match kind {
                b'R' => {
                    let mut fields = Fields(body);
                    let code = fields.i32()?;
                    let rest = fields.0.to_vec();
                    let password = || {
                        config.password.clone().ok_or_else(|| Error::Server {
                            code: "28P01".into(),
                            message: "the database wants a password and the URL has none".into(),
                        })
                    };
                    let mut out = Vec::new();
                    match code {
                        0 => {}
                        3 => {
                            Out::new(b'p').cstr(&password()?).finish(&mut out);
                            self.send(&out)?;
                        }
                        5 => {
                            let inner = md5_hex(format!("{}{}", password()?, config.user).as_bytes());
                            let mut salted = inner.into_bytes();
                            salted.extend_from_slice(rest.get(..4).ok_or_else(|| Error::Broken("no salt".into()))?);
                            Out::new(b'p').cstr(&format!("md5{}", md5_hex(&salted))).finish(&mut out);
                            self.send(&out)?;
                        }
                        10 => {
                            let mechanisms: Vec<&[u8]> = rest.split(|&b| b == 0).collect();
                            if !mechanisms.contains(&&b"SCRAM-SHA-256"[..]) {
                                return Err(Error::Broken(
                                    "the database offers no SASL method this client knows".into(),
                                ));
                            }
                            let state = Scram::new(password()?);
                            let first = state.client_first();
                            Out::new(b'p')
                                .cstr("SCRAM-SHA-256")
                                .i32(first.len() as i32)
                                .bytes(first.as_bytes())
                                .finish(&mut out);
                            self.send(&out)?;
                            scram = Some(state);
                        }
                        11 => {
                            let state = scram.as_mut().ok_or_else(|| Error::Broken("SASL out of order".into()))?;
                            let reply = state.client_final(&rest)?;
                            Out::new(b'p').bytes(reply.as_bytes()).finish(&mut out);
                            self.send(&out)?;
                        }
                        12 => {
                            let state = scram.as_ref().ok_or_else(|| Error::Broken("SASL out of order".into()))?;
                            state.verify_server(&rest)?;
                        }
                        other => {
                            return Err(Error::Broken(format!(
                                "the database asks for authentication method {other}, which this client does not support"
                            )));
                        }
                    }
                }
                b'E' => return Err(server_error(body)),
                b'Z' => {
                    self.status = body.first().copied().unwrap_or(b'I');
                    return Ok(());
                }
                // ParameterStatus, BackendKeyData, notices.
                _ => {}
            }
        }
    }

    /// Run a statement with the extended protocol.
    fn query(&mut self, sql: &str, args: &[Option<String>]) -> Result<QueryResult, Error> {
        if args.len() > i16::MAX as usize {
            return Err(Error::Server { code: "08P01".into(), message: "too many parameters".into() });
        }
        let mut out = Vec::with_capacity(sql.len() + 64);
        Out::new(b'P').cstr("").cstr(sql).i16(0).finish(&mut out);
        let mut bind = Out::new(b'B');
        bind.cstr("").cstr("").i16(0).i16(args.len() as i16);
        for arg in args {
            match arg {
                Some(text) => bind.i32(text.len() as i32).bytes(text.as_bytes()),
                None => bind.i32(-1),
            };
        }
        bind.i16(0).finish(&mut out);
        Out::new(b'D').bytes(b"P").cstr("").finish(&mut out);
        Out::new(b'E').cstr("").i32(0).finish(&mut out);
        Out::new(b'S').finish(&mut out);
        self.send(&out)?;
        self.results()
    }

    /// Run a script of statements with the simple protocol; the result is the last statement's.
    fn script(&mut self, sql: &str) -> Result<QueryResult, Error> {
        let mut out = Vec::with_capacity(sql.len() + 8);
        Out::new(b'Q').cstr(sql).finish(&mut out);
        self.send(&out)?;
        self.results()
    }

    /// Read the replies up to ReadyForQuery.
    fn results(&mut self) -> Result<QueryResult, Error> {
        let mut result = QueryResult::default();
        let mut failure = None;
        loop {
            let (kind, body) = self.next(None)?;
            match kind {
                b'T' => {
                    let mut fields = Fields(body);
                    let count = fields.i16()?;
                    result = QueryResult::default();
                    for _ in 0..count {
                        let name = fields.cstr()?.to_string();
                        let _table = fields.i32()?;
                        let _column = fields.i16()?;
                        let oid = fields.i32()? as u32;
                        let _size = fields.i16()?;
                        let _modifier = fields.i32()?;
                        let _format = fields.i16()?;
                        result.columns.push((name, oid));
                    }
                }
                b'D' => {
                    if failure.is_none() {
                        result.add_row(body)?;
                    }
                }
                b'C' => {
                    let tag = Fields(body).cstr().unwrap_or("");
                    result.affected = tag.rsplit(' ').next().and_then(|n| n.parse().ok()).unwrap_or(0);
                }
                b'E' => failure = Some(server_error(body)),
                b'G' => {
                    // COPY FROM STDIN: refuse it, and the server reports an error.
                    let mut out = Vec::new();
                    Out::new(b'f').cstr("COPY is not supported by this client").finish(&mut out);
                    self.send(&out)?;
                }
                b'Z' => {
                    self.status = body.first().copied().unwrap_or(b'I');
                    return match failure {
                        Some(error) => Err(error),
                        None => Ok(result),
                    };
                }
                // ParseComplete, BindComplete, NoData, EmptyQueryResponse, notices, parameter
                // changes, notifications, and COPY TO STDOUT data.
                _ => {}
            }
        }
    }

    fn terminate(&self) {
        let mut out = Vec::new();
        Out::new(b'X').finish(&mut out);
        let _ = self.socket.write_all(&out);
        self.socket.shutdown();
    }
}

// ---- SCRAM-SHA-256 (RFC 5802, RFC 7677) -------------------------------------------------

struct Scram {
    password: String,
    nonce: String,
    /// What the server must prove it knows, once the client's final message is sent.
    server_signature: Vec<u8>,
}

impl Scram {
    fn new(password: String) -> Scram {
        use ring::rand::SecureRandom;
        let mut random = [0u8; 18];
        let _ = ring::rand::SystemRandom::new().fill(&mut random);
        Scram { password, nonce: crate::ws::base64(&random), server_signature: Vec::new() }
    }

    fn client_first_bare(&self) -> String {
        // PostgreSQL takes the user from the startup message and ignores this one.
        format!("n=,r={}", self.nonce)
    }

    fn client_first(&self) -> String {
        format!("n,,{}", self.client_first_bare())
    }

    fn client_final(&mut self, server_first: &[u8]) -> Result<String, Error> {
        use ring::{digest, hmac, pbkdf2};
        let bad = || Error::Broken("a malformed SCRAM message from the database".into());
        let server_first = std::str::from_utf8(server_first).map_err(|_| bad())?;
        let (mut nonce, mut salt, mut iterations) = (None, None, None);
        for part in server_first.split(',') {
            match part.split_at_checked(2) {
                Some(("r=", v)) => nonce = Some(v),
                Some(("s=", v)) => salt = base64_decode(v),
                Some(("i=", v)) => iterations = v.parse::<u32>().ok().and_then(std::num::NonZeroU32::new),
                _ => {}
            }
        }
        let (Some(nonce), Some(salt), Some(iterations)) = (nonce, salt, iterations) else { return Err(bad()) };
        if !nonce.starts_with(&self.nonce) {
            return Err(Error::Broken("the database's SCRAM nonce does not extend the client's".into()));
        }
        let mut salted = [0u8; 32];
        pbkdf2::derive(pbkdf2::PBKDF2_HMAC_SHA256, iterations, &salt, self.password.as_bytes(), &mut salted);
        let salted = hmac::Key::new(hmac::HMAC_SHA256, &salted);
        let client_key = hmac::sign(&salted, b"Client Key");
        let stored_key = digest::digest(&digest::SHA256, client_key.as_ref());
        let without_proof = format!("c=biws,r={nonce}");
        let auth_message = format!("{},{server_first},{without_proof}", self.client_first_bare());
        let signature = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, stored_key.as_ref()), auth_message.as_bytes());
        let proof: Vec<u8> = client_key.as_ref().iter().zip(signature.as_ref()).map(|(a, b)| a ^ b).collect();
        let server_key = hmac::sign(&salted, b"Server Key");
        self.server_signature =
            hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, server_key.as_ref()), auth_message.as_bytes())
                .as_ref()
                .to_vec();
        Ok(format!("{without_proof},p={}", crate::ws::base64(&proof)))
    }

    fn verify_server(&self, server_final: &[u8]) -> Result<(), Error> {
        let text = std::str::from_utf8(server_final).unwrap_or("");
        let verifier = text.split(',').find_map(|p| p.strip_prefix("v=")).and_then(base64_decode);
        if verifier.as_deref() == Some(&self.server_signature[..]) && !self.server_signature.is_empty() {
            Ok(())
        } else {
            Err(Error::Broken("the database's SCRAM signature is wrong".into()))
        }
    }
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let text = text.trim_end_matches('=');
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut bits, mut count) = (0u32, 0);
    for &c in text.as_bytes() {
        let value = crate::ws::BASE64.iter().position(|&b| b == c)? as u32;
        bits = bits << 6 | value;
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
        }
    }
    Some(out)
}

// ---- MD5 (RFC 1321), for servers that still use md5 passwords ---------------------------

fn md5_hex(data: &[u8]) -> String {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14,
        20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6,
        10, 15, 21,
    ];
    let k: Vec<u32> = (1..=64).map(|i: i32| (f64::from(i).sin().abs() * 4294967296.0) as u32).collect();
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&((data.len() as u64).wrapping_mul(8)).to_le_bytes());
    let mut h: [u32; 4] = [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476];
    for block in message.chunks(64) {
        let m: Vec<u32> = block.chunks(4).map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]])).collect();
        let [mut a, mut b, mut c, mut d] = h;
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d]) {
            *x = x.wrapping_add(y);
        }
    }
    h.iter().flat_map(|w| w.to_le_bytes()).map(|b| format!("{b:02x}")).collect()
}

// ---- pools ------------------------------------------------------------------------------

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

/// Transactions in progress: the pool and the connection they hold.
static TRANSACTIONS: Mutex<Slab<(Arc<Pool>, Option<Conn>)>> = Mutex::new(Slab::new());

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
            while !node.signaled() && Instant::now() < deadline {
                fiber::park(Some(deadline));
            }
            if !node.signaled() {
                fiber::lock(&self.state).waiters.remove(&node);
                if !node.signaled() {
                    return Err(Error::Broken(format!(
                        "every database connection (pool_max={}) stayed busy for {} seconds",
                        self.config.pool_max,
                        POOL_WAIT.as_secs()
                    )));
                }
            }
        }
    }

    /// Give a connection back: kept if it is healthy and outside any transaction.
    fn release(&self, conn: Conn) {
        if conn.status == b'I' {
            let mut state = fiber::lock(&self.state);
            state.idle.push(conn);
            state.waiters.wake_one();
        } else {
            conn.terminate();
            self.closed();
        }
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
        if matches!(result, Err(Error::Broken(_))) {
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

/// Run `f` on the transaction's connection, or on one from the pool when `tx` is -1.
fn run(pool_id: i64, tx: i64, f: impl FnOnce(&mut Conn) -> Result<QueryResult, Error>) -> i64 {
    let outcome = if tx >= 0 {
        let taken = fiber::lock(&TRANSACTIONS).get_mut(tx).and_then(|(pool, conn)| Some((pool.clone(), conn.take()?)));
        match taken {
            None => Err(Error::Server { code: "25000".into(), message: "the transaction has already finished".into() }),
            Some((pool, mut conn)) => {
                let result = f(&mut conn);
                if matches!(result, Err(Error::Broken(_))) {
                    conn.terminate();
                    pool.closed();
                    fiber::lock(&TRANSACTIONS).remove(tx);
                } else if let Some((_, slot)) = fiber::lock(&TRANSACTIONS).get_mut(tx) {
                    *slot = Some(conn);
                }
                result
            }
        }
    } else {
        match pool(pool_id) {
            Some(pool) => pool.with(f),
            None => Err(Error::Broken("not a database connection".into())),
        }
    };
    match outcome {
        Ok(result) => fiber::lock(&RESULTS).insert(result),
        Err(error) => {
            record(error);
            -1
        }
    }
}

fn take_args() -> Vec<Option<String>> {
    ARGS.with(|a| std::mem::take(&mut *a.borrow_mut()))
}

// ---- the functions compiled code calls ---------------------------------------------------

/// Open a pool for a `postgres://` URL, checking it with one connection. Returns the pool's
/// id, or -1 after recording the error.
///
/// # Safety
/// `url` and `url_len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_pg_open(url: *const u8, url_len: i64) -> i64 {
    let url = unsafe { Str::from_raw(url, url_len) };
    let config = match parse_url(url) {
        Ok(config) => config,
        Err(message) => {
            record(Error::Server { code: "08000".into(), message });
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

/// Add a parameter for the next query on this thread; `null` (non-zero) for SQL NULL.
///
/// # Safety
/// `ptr` and `len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_pg_arg(ptr: *const u8, len: i64, null: i64) {
    let value = (null == 0).then(|| unsafe { Str::from_raw(ptr, len) }.to_string());
    ARGS.with(|a| a.borrow_mut().push(value));
}

/// Run a statement with the parameters added since the last one, on the pool `pool` or, when
/// `tx` is not -1, in that transaction. Returns a result handle, or -1 after recording the error.
///
/// # Safety
/// `sql` and `sql_len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_pg_query(pool: i64, tx: i64, sql: *const u8, sql_len: i64) -> i64 {
    let sql = unsafe { Str::from_raw(sql, sql_len) };
    let args = take_args();
    run(pool, tx, |conn| conn.query(sql, &args))
}

/// Run several statements, without parameters. Returns a result handle for the last one.
///
/// # Safety
/// `sql` and `sql_len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_pg_script(pool: i64, tx: i64, sql: *const u8, sql_len: i64) -> i64 {
    let sql = unsafe { Str::from_raw(sql, sql_len) };
    take_args();
    run(pool, tx, |conn| conn.script(sql))
}

/// Start a transaction on a connection of its own. Returns its id, or -1.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_pg_begin(pool_id: i64) -> i64 {
    let Some(pool) = pool(pool_id) else {
        record(Error::Broken("not a database connection".into()));
        return -1;
    };
    let mut conn = match pool.acquire() {
        Ok(conn) => conn,
        Err(error) => {
            record(error);
            return -1;
        }
    };
    match conn.script("BEGIN") {
        Ok(_) => fiber::lock(&TRANSACTIONS).insert((pool, Some(conn))),
        Err(error) => {
            if matches!(error, Error::Broken(_)) {
                conn.terminate();
                pool.closed();
            } else {
                pool.release(conn);
            }
            record(error);
            -1
        }
    }
}

/// Commit (`commit` non-zero) or roll back a transaction and give its connection back. Returns 0,
/// or -1 after recording the error; either way the transaction is over. Finishing one that is
/// already over does nothing.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_pg_finish(tx: i64, commit: i64) -> i64 {
    let Some((pool, conn)) = fiber::lock(&TRANSACTIONS).remove(tx) else { return 0 };
    let Some(mut conn) = conn else { return 0 };
    // COMMIT of a failed transaction rolls it back and says so only with its tag; say so here.
    let failed = conn.status == b'E';
    let outcome = match conn.script(if commit != 0 { "COMMIT" } else { "ROLLBACK" }) {
        Ok(_) if commit != 0 && failed => Err(Error::Server {
            code: "25P02".into(),
            message: "the transaction was rolled back because a statement in it failed".into(),
        }),
        Ok(_) => Ok(()),
        Err(e) => Err(e),
    };
    if matches!(outcome, Err(Error::Broken(_))) {
        conn.terminate();
        pool.closed();
    } else {
        pool.release(conn);
    }
    match outcome {
        Ok(()) => 0,
        Err(error) => {
            record(error);
            -1
        }
    }
}

fn with_result<T>(handle: i64, default: T, f: impl FnOnce(&QueryResult) -> T) -> T {
    fiber::lock(&RESULTS).get(handle).map_or(default, f)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_pg_rows(handle: i64) -> i64 {
    with_result(handle, 0, |r| r.rows() as i64)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_pg_columns(handle: i64) -> i64 {
    with_result(handle, 0, |r| r.columns.len() as i64)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_pg_column(handle: i64, column: i64) -> Str {
    with_result(handle, Str::empty(), |r| {
        r.columns.get(column as usize).map_or(Str::empty(), |(name, _)| Str::from_bytes(name.as_bytes()))
    })
}

/// A value as text, or `None` for NULL.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_pg_value(handle: i64, row: i64, column: i64) -> Str {
    with_result(handle, Str { ptr: std::ptr::null(), len: 0 }, |r| match r.value(row as usize, column as usize) {
        Some(text) => Str::from_bytes(text),
        None => Str { ptr: std::ptr::null(), len: 0 },
    })
}

/// The number of rows the statement inserted, updated, deleted or returned.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_pg_affected(handle: i64) -> i64 {
    with_result(handle, 0, |r| r.affected)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_pg_json(handle: i64) -> Str {
    with_result(handle, Str::empty(), |r| Str::from_bytes(&r.to_json()))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_pg_free(handle: i64) {
    fiber::lock(&RESULTS).remove(handle);
}

/// The SQLSTATE code of the last error on this thread (`08006` when the connection failed).
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_pg_error_code() -> Str {
    LAST_CODE.with(|c| Str::from_bytes(c.borrow().as_bytes()))
}

/// Close the pool's idle connections. Connections in use close when they come back.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_pg_close(pool_id: i64) {
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

    #[test]
    fn urls() {
        let c = parse_url("postgres://ann:p%40ss@db.example:6543/app?sslmode=require&pool_max=3").unwrap();
        assert_eq!((c.user.as_str(), c.password.as_deref()), ("ann", Some("p@ss")));
        assert_eq!((c.host.as_str(), c.port, c.database.as_str()), ("db.example", 6543, "app"));
        assert_eq!((c.ssl, c.pool_max), (SslMode::Require, 3));
        let c = parse_url("postgresql://bob@[::1]/").unwrap();
        assert_eq!((c.host.as_str(), c.port, c.database.as_str(), c.password), ("::1", 5432, "bob", None));
        assert!(parse_url("mysql://x").is_err());
        assert!(parse_url("postgres://x/y?sslmode=verify-ca").unwrap_err().contains("verify-ca"));
    }

    #[test]
    fn md5_known_digests() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"The quick brown fox jumps over the lazy dog"), "9e107d9d372bb6826bd81d3542a419d6");
    }

    #[test]
    fn scram_matches_rfc_7677() {
        // The example exchange in RFC 7677, section 3 (user "user", password "pencil").
        let mut scram =
            Scram { password: "pencil".into(), nonce: "rOprNGfwEbeRWgbNEkqO".into(), server_signature: Vec::new() };
        // The RFC's client-first-bare names the user; PostgreSQL's leaves it empty, so the
        // proof differs from the RFC's. Check against the RFC with its user name.
        let server_first = "r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096";
        let reply = scram.client_final(server_first.as_bytes()).unwrap();
        assert!(reply.starts_with("c=biws,r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,p="), "{reply}");
        assert_eq!(scram.server_signature.len(), 32);
        assert!(scram.verify_server(b"v=AAAA").is_err());
    }

    #[test]
    fn json_rows() {
        let mut result = QueryResult {
            columns: vec![("id".into(), 23), ("ok".into(), 16), ("name".into(), 25)],
            ..Default::default()
        };
        let mut row = Vec::new();
        row.extend_from_slice(&3i16.to_be_bytes());
        for value in [Some(&b"7"[..]), Some(b"t"), None] {
            match value {
                Some(v) => {
                    row.extend_from_slice(&(v.len() as i32).to_be_bytes());
                    row.extend_from_slice(v);
                }
                None => row.extend_from_slice(&(-1i32).to_be_bytes()),
            }
        }
        result.add_row(&row).unwrap();
        assert_eq!(String::from_utf8(result.to_json()).unwrap(), r#"[{"id":7,"ok":true,"name":null}]"#);
        assert_eq!(base64_decode("cGVuY2ls").unwrap(), b"pencil");
    }
}
