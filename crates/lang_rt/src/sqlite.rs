//! SQLite (docs/14, milestone 17), through hand-written FFI to the system's `libsqlite3`.
//!
//! - **The library is loaded when the first database is opened**, with `dlopen`, so a program that
//!   never opens one links nothing and needs nothing installed (decision D122; the plan had
//!   `-lsqlite3` at link time, which would also need the `-dev` package's symlink).
//! - **Each database has a thread of its own** that owns the connection. SQLite calls block (on
//!   the disk, and on another process's lock for up to `busy_timeout`), so tasks send the thread
//!   work and park until it answers; worker threads never wait on SQLite.
//! - **A transaction owns the connection** until it ends: the thread holds back other work meanwhile
//!   and carries on with it, in order, afterwards.
//! - A database opens with `journal_mode=WAL` (not for `:memory:`), `busy_timeout=5000` and
//!   `foreign_keys=ON`.
//!
//! Results are kept in a table until the standard library has copied them, as in `pg.rs`. The
//! last error's name (`SQLITE_CONSTRAINT_UNIQUE`) is kept per thread, like its message.

use crate::fiber::{self, Waker};
use crate::json::push_json_string;
use crate::{Str, sys};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::ffi::{CStr, CString, c_char, c_double, c_int, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};

// ---- the library ------------------------------------------------------------------------

type Db = c_void;
type Stmt = c_void;

const SQLITE_OK: c_int = 0;
const SQLITE_ROW: c_int = 100;
const SQLITE_DONE: c_int = 101;
const SQLITE_OPEN_READWRITE: c_int = 0x2;
const SQLITE_OPEN_CREATE: c_int = 0x4;
const SQLITE_OPEN_FULLMUTEX: c_int = 0x10000;
const SQLITE_OPEN_URI: c_int = 0x40;
const SQLITE_INTEGER: c_int = 1;
const SQLITE_FLOAT: c_int = 2;
const SQLITE_TEXT: c_int = 3;
const SQLITE_BLOB: c_int = 4;

#[cfg(unix)]
unsafe extern "C" {
    fn dlopen(file: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
}

/// The functions of `libsqlite3` that are used.
struct Api {
    open_v2: unsafe extern "C" fn(*const c_char, *mut *mut Db, c_int, *const c_char) -> c_int,
    close_v2: unsafe extern "C" fn(*mut Db) -> c_int,
    exec: unsafe extern "C" fn(*mut Db, *const c_char, *const c_void, *const c_void, *mut *mut c_char) -> c_int,
    free: unsafe extern "C" fn(*mut c_void),
    prepare_v2: unsafe extern "C" fn(*mut Db, *const c_char, c_int, *mut *mut Stmt, *mut *const c_char) -> c_int,
    step: unsafe extern "C" fn(*mut Stmt) -> c_int,
    finalize: unsafe extern "C" fn(*mut Stmt) -> c_int,
    bind_null: unsafe extern "C" fn(*mut Stmt, c_int) -> c_int,
    bind_int64: unsafe extern "C" fn(*mut Stmt, c_int, i64) -> c_int,
    bind_double: unsafe extern "C" fn(*mut Stmt, c_int, c_double) -> c_int,
    // The last argument is SQLITE_TRANSIENT, -1: a destructor that is not a pointer.
    bind_text: unsafe extern "C" fn(*mut Stmt, c_int, *const c_char, c_int, isize) -> c_int,
    bind_blob: unsafe extern "C" fn(*mut Stmt, c_int, *const c_void, c_int, isize) -> c_int,
    bind_parameter_count: unsafe extern "C" fn(*mut Stmt) -> c_int,
    column_count: unsafe extern "C" fn(*mut Stmt) -> c_int,
    column_name: unsafe extern "C" fn(*mut Stmt, c_int) -> *const c_char,
    column_decltype: unsafe extern "C" fn(*mut Stmt, c_int) -> *const c_char,
    column_type: unsafe extern "C" fn(*mut Stmt, c_int) -> c_int,
    column_int64: unsafe extern "C" fn(*mut Stmt, c_int) -> i64,
    column_double: unsafe extern "C" fn(*mut Stmt, c_int) -> c_double,
    column_text: unsafe extern "C" fn(*mut Stmt, c_int) -> *const u8,
    column_blob: unsafe extern "C" fn(*mut Stmt, c_int) -> *const u8,
    column_bytes: unsafe extern "C" fn(*mut Stmt, c_int) -> c_int,
    errmsg: unsafe extern "C" fn(*mut Db) -> *const c_char,
    extended_errcode: unsafe extern "C" fn(*mut Db) -> c_int,
    changes: unsafe extern "C" fn(*mut Db) -> c_int,
    last_insert_rowid: unsafe extern "C" fn(*mut Db) -> i64,
    busy_timeout: unsafe extern "C" fn(*mut Db, c_int) -> c_int,
    extended_result_codes: unsafe extern "C" fn(*mut Db, c_int) -> c_int,
    get_autocommit: unsafe extern "C" fn(*mut Db) -> c_int,
}

static API: OnceLock<Result<Api, String>> = OnceLock::new();

#[cfg(unix)]
fn load() -> Result<Api, String> {
    const NAMES: &[&str] = &["libsqlite3.so.0", "libsqlite3.so", "libsqlite3.0.dylib", "libsqlite3.dylib"];
    let mut library = std::ptr::null_mut();
    for name in NAMES {
        let name = CString::new(*name).unwrap();
        // RTLD_NOW.
        library = unsafe { dlopen(name.as_ptr(), 2) };
        if !library.is_null() {
            break;
        }
    }
    if library.is_null() {
        return Err("SQLite is not installed: the library libsqlite3 was not found (on Debian and Ubuntu, `apt install libsqlite3-0`)".into());
    }
    fn find<T: Copy>(library: *mut c_void, name: &str) -> Result<T, String> {
        let c_name = CString::new(name).unwrap();
        let address = unsafe { dlsym(library, c_name.as_ptr()) };
        if address.is_null() {
            return Err(format!("the installed libsqlite3 has no `{name}`"));
        }
        assert_eq!(std::mem::size_of::<T>(), std::mem::size_of::<*mut c_void>());
        // SAFETY: the symbol has the signature of the field it is stored in, and `T` is a function
        // pointer, as large as `address`.
        Ok(unsafe { std::mem::transmute_copy::<*mut c_void, T>(&address) })
    }
    macro_rules! symbol {
        ($name:literal) => {
            find(library, $name)?
        };
    }
    Ok(Api {
        open_v2: symbol!("sqlite3_open_v2"),
        close_v2: symbol!("sqlite3_close_v2"),
        exec: symbol!("sqlite3_exec"),
        free: symbol!("sqlite3_free"),
        prepare_v2: symbol!("sqlite3_prepare_v2"),
        step: symbol!("sqlite3_step"),
        finalize: symbol!("sqlite3_finalize"),
        bind_null: symbol!("sqlite3_bind_null"),
        bind_int64: symbol!("sqlite3_bind_int64"),
        bind_double: symbol!("sqlite3_bind_double"),
        bind_text: symbol!("sqlite3_bind_text"),
        bind_blob: symbol!("sqlite3_bind_blob"),
        bind_parameter_count: symbol!("sqlite3_bind_parameter_count"),
        column_count: symbol!("sqlite3_column_count"),
        column_name: symbol!("sqlite3_column_name"),
        column_decltype: symbol!("sqlite3_column_decltype"),
        column_type: symbol!("sqlite3_column_type"),
        column_int64: symbol!("sqlite3_column_int64"),
        column_double: symbol!("sqlite3_column_double"),
        column_text: symbol!("sqlite3_column_text"),
        column_blob: symbol!("sqlite3_column_blob"),
        column_bytes: symbol!("sqlite3_column_bytes"),
        errmsg: symbol!("sqlite3_errmsg"),
        extended_errcode: symbol!("sqlite3_extended_errcode"),
        changes: symbol!("sqlite3_changes"),
        last_insert_rowid: symbol!("sqlite3_last_insert_rowid"),
        busy_timeout: symbol!("sqlite3_busy_timeout"),
        extended_result_codes: symbol!("sqlite3_extended_result_codes"),
        get_autocommit: symbol!("sqlite3_get_autocommit"),
    })
}

#[cfg(not(unix))]
fn load() -> Result<Api, String> {
    Err("SQLite is not supported on this platform yet".into())
}

fn api() -> Result<&'static Api, String> {
    API.get_or_init(load).as_ref().map_err(|e| e.clone())
}

// ---- errors -----------------------------------------------------------------------------

/// A failed call: the code's name and SQLite's message.
#[derive(Clone, Debug)]
struct Error {
    code: String,
    message: String,
}

/// The name of a (possibly extended) result code.
fn code_name(code: c_int) -> String {
    let named = match code {
        2067 => "SQLITE_CONSTRAINT_UNIQUE",
        1555 => "SQLITE_CONSTRAINT_PRIMARYKEY",
        787 => "SQLITE_CONSTRAINT_FOREIGNKEY",
        1299 => "SQLITE_CONSTRAINT_NOTNULL",
        275 => "SQLITE_CONSTRAINT_CHECK",
        5 => "SQLITE_BUSY",
        261 => "SQLITE_BUSY_RECOVERY",
        517 => "SQLITE_BUSY_SNAPSHOT",
        6 => "SQLITE_LOCKED",
        262 => "SQLITE_LOCKED_SHAREDCACHE",
        _ => "",
    };
    if !named.is_empty() {
        return named.to_string();
    }
    let primary = match code & 0xff {
        1 => "SQLITE_ERROR",
        2 => "SQLITE_INTERNAL",
        3 => "SQLITE_PERM",
        4 => "SQLITE_ABORT",
        7 => "SQLITE_NOMEM",
        8 => "SQLITE_READONLY",
        9 => "SQLITE_INTERRUPT",
        10 => "SQLITE_IOERR",
        11 => "SQLITE_CORRUPT",
        12 => "SQLITE_NOTFOUND",
        13 => "SQLITE_FULL",
        14 => "SQLITE_CANTOPEN",
        15 => "SQLITE_PROTOCOL",
        17 => "SQLITE_SCHEMA",
        18 => "SQLITE_TOOBIG",
        19 => "SQLITE_CONSTRAINT",
        20 => "SQLITE_MISMATCH",
        21 => "SQLITE_MISUSE",
        23 => "SQLITE_AUTH",
        25 => "SQLITE_RANGE",
        26 => "SQLITE_NOTADB",
        _ => "SQLITE_ERROR",
    };
    primary.to_string()
}

thread_local! {
    static LAST_CODE: RefCell<String> = const { RefCell::new(String::new()) };
    static ARGS: RefCell<Vec<Arg>> = const { RefCell::new(Vec::new()) };
}

fn record(error: Error) {
    LAST_CODE.with(|c| *c.borrow_mut() = error.code);
    sys::record_error(error.message);
}

fn failure(message: impl Into<String>) -> Error {
    Error { code: "SQLITE_ERROR".into(), message: message.into() }
}

// ---- values and results -----------------------------------------------------------------

#[derive(Clone, Debug)]
enum Arg {
    Null,
    Int(i64),
    Float(f64),
    Text(String),
    Blob(Vec<u8>),
}

#[derive(Clone, Debug)]
enum Cell {
    Null,
    Int(i64),
    Float(f64),
    Text(Vec<u8>),
    Blob(Vec<u8>),
}

#[derive(Default, Debug)]
struct QueryResult {
    columns: Vec<String>,
    /// Each column's declared type, upper-cased, for turning `BOOLEAN` columns into booleans.
    declared: Vec<String>,
    rows: Vec<Vec<Cell>>,
    affected: i64,
    last_insert_id: i64,
}

impl QueryResult {
    fn to_json(&self) -> Vec<u8> {
        let mut out = vec![b'['];
        for (r, row) in self.rows.iter().enumerate() {
            if r > 0 {
                out.push(b',');
            }
            out.push(b'{');
            for (c, cell) in row.iter().enumerate() {
                if c > 0 {
                    out.push(b',');
                }
                push_json_string(&mut out, self.columns[c].as_bytes());
                out.push(b':');
                match cell {
                    Cell::Null => out.extend_from_slice(b"null"),
                    Cell::Int(i) if self.declared[c].contains("BOOL") => {
                        out.extend_from_slice(if *i != 0 { b"true" } else { b"false" });
                    }
                    Cell::Int(i) => out.extend_from_slice(i.to_string().as_bytes()),
                    Cell::Float(f) if f.is_finite() => out.extend_from_slice(format!("{f:?}").as_bytes()),
                    Cell::Float(_) => out.extend_from_slice(b"null"),
                    Cell::Text(text) => push_json_string(&mut out, text),
                    Cell::Blob(data) => {
                        push_json_string(&mut out, crate::ws::base64(data).as_bytes());
                    }
                }
            }
            out.push(b'}');
        }
        out.push(b']');
        out
    }
}

/// A slab of ids to values, as in `pg.rs`.
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

static RESULTS: Mutex<Slab<QueryResult>> = Mutex::new(Slab::new());

// ---- the connection, on its thread ------------------------------------------------------

struct Connection {
    api: &'static Api,
    db: *mut Db,
}

// SAFETY: the connection is used only by the thread that owns it.
unsafe impl Send for Connection {}

impl Connection {
    fn error(&self) -> Error {
        // SAFETY: `db` is open.
        let (code, message) = unsafe {
            let code = (self.api.extended_errcode)(self.db);
            let message = CStr::from_ptr((self.api.errmsg)(self.db)).to_string_lossy().into_owned();
            (code, message)
        };
        Error { code: code_name(code), message }
    }

    fn open(path: &str) -> Result<Connection, Error> {
        let api = api().map_err(failure)?;
        let c_path = CString::new(path).map_err(|_| failure("the database path holds a NUL character"))?;
        let mut db = std::ptr::null_mut();
        let flags = SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE | SQLITE_OPEN_FULLMUTEX | SQLITE_OPEN_URI;
        // SAFETY: the arguments are valid for the call.
        let status = unsafe { (api.open_v2)(c_path.as_ptr(), &mut db, flags, std::ptr::null()) };
        let connection = Connection { api, db };
        if status != SQLITE_OK || db.is_null() {
            let error = if db.is_null() {
                Error { code: code_name(status), message: "out of memory".into() }
            } else {
                let mut error = connection.error();
                error.message = format!("cannot open {path}: {}", error.message);
                // SAFETY: `db` came from `open_v2`.
                unsafe { (api.close_v2)(db) };
                error
            };
            return Err(error);
        }
        // SAFETY: `db` is open.
        unsafe {
            (api.extended_result_codes)(db, 1);
            (api.busy_timeout)(db, 5000);
        }
        let memory = path == ":memory:" || path.is_empty() || path.contains("mode=memory");
        if !memory {
            // Switching a new file to WAL needs a lock that the busy timeout does not always wait
            // for, when another process is switching it or writing at the same moment: two
            // instances starting together. Try again for as long as the timeout would have.
            let mut tries = 0;
            loop {
                match connection.script("PRAGMA journal_mode=WAL") {
                    Err(error) if error.code.starts_with("SQLITE_BUSY") && tries < 100 => {
                        tries += 1;
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                    other => break other?,
                }
            }
        }
        connection.script("PRAGMA foreign_keys=ON")?;
        Ok(connection)
    }

    /// Run statements without parameters or results.
    fn script(&self, sql: &str) -> Result<(), Error> {
        let c_sql = CString::new(sql).map_err(|_| failure("the SQL holds a NUL character"))?;
        let mut message: *mut c_char = std::ptr::null_mut();
        // SAFETY: the arguments are valid for the call; `message` is freed with SQLite's `free`.
        let status =
            unsafe { (self.api.exec)(self.db, c_sql.as_ptr(), std::ptr::null(), std::ptr::null(), &mut message) };
        if status == SQLITE_OK {
            return Ok(());
        }
        let error = self.error();
        if !message.is_null() {
            // SAFETY: allocated by SQLite.
            unsafe { (self.api.free)(message.cast()) };
        }
        Err(error)
    }

    fn query(&self, sql: &str, args: &[Arg]) -> Result<QueryResult, Error> {
        let api = self.api;
        let c_sql = CString::new(sql).map_err(|_| failure("the SQL holds a NUL character"))?;
        let mut stmt = std::ptr::null_mut();
        // SAFETY: the arguments are valid for the call.
        let status = unsafe { (api.prepare_v2)(self.db, c_sql.as_ptr(), -1, &mut stmt, std::ptr::null_mut()) };
        if status != SQLITE_OK {
            return Err(self.error());
        }
        if stmt.is_null() {
            // Nothing but white space or a comment.
            return Ok(QueryResult::default());
        }
        let outcome = self.run_statement(stmt, args);
        // SAFETY: `stmt` came from `prepare_v2`.
        unsafe { (api.finalize)(stmt) };
        outcome
    }

    fn run_statement(&self, stmt: *mut Stmt, args: &[Arg]) -> Result<QueryResult, Error> {
        let api = self.api;
        // SAFETY: `stmt` is a prepared statement.
        let wanted = unsafe { (api.bind_parameter_count)(stmt) } as usize;
        if wanted != args.len() {
            return Err(Error {
                code: "SQLITE_RANGE".into(),
                message: format!(
                    "the statement has {wanted} parameter{} (`?`) but {} value{} given",
                    if wanted == 1 { "" } else { "s" },
                    args.len(),
                    if args.len() == 1 { " was" } else { "s were" },
                ),
            });
        }
        for (i, arg) in args.iter().enumerate() {
            let at = i as c_int + 1;
            // SAFETY: `stmt` is a prepared statement and the data lasts for the call (SQLite copies it).
            let status = unsafe {
                match arg {
                    Arg::Null => (api.bind_null)(stmt, at),
                    Arg::Int(n) => (api.bind_int64)(stmt, at, *n),
                    Arg::Float(f) => (api.bind_double)(stmt, at, *f),
                    Arg::Text(text) => (api.bind_text)(stmt, at, text.as_ptr().cast(), text.len() as c_int, -1),
                    Arg::Blob(data) => (api.bind_blob)(stmt, at, data.as_ptr().cast(), data.len() as c_int, -1),
                }
            };
            if status != SQLITE_OK {
                return Err(self.error());
            }
        }
        // SAFETY: `stmt` is a prepared statement.
        let columns = unsafe { (api.column_count)(stmt) } as usize;
        let mut result = QueryResult::default();
        for c in 0..columns {
            // SAFETY: as above; the strings are valid until the next call on the statement.
            let (name, declared) = unsafe {
                let name = CStr::from_ptr((api.column_name)(stmt, c as c_int)).to_string_lossy().into_owned();
                let declared = (api.column_decltype)(stmt, c as c_int);
                let declared = if declared.is_null() {
                    String::new()
                } else {
                    CStr::from_ptr(declared).to_string_lossy().to_uppercase()
                };
                (name, declared)
            };
            result.columns.push(name);
            result.declared.push(declared);
        }
        loop {
            // SAFETY: `stmt` is a prepared statement.
            let status = unsafe { (api.step)(stmt) };
            match status {
                SQLITE_ROW => {
                    let mut row = Vec::with_capacity(columns);
                    for c in 0..columns as c_int {
                        // SAFETY: the column is in range; each pointer is read before the next call.
                        let cell = unsafe {
                            match (api.column_type)(stmt, c) {
                                SQLITE_INTEGER => Cell::Int((api.column_int64)(stmt, c)),
                                SQLITE_FLOAT => Cell::Float((api.column_double)(stmt, c)),
                                SQLITE_TEXT => {
                                    let ptr = (api.column_text)(stmt, c);
                                    let len = (api.column_bytes)(stmt, c) as usize;
                                    Cell::Text(if ptr.is_null() {
                                        Vec::new()
                                    } else {
                                        std::slice::from_raw_parts(ptr, len).to_vec()
                                    })
                                }
                                SQLITE_BLOB => {
                                    let ptr = (api.column_blob)(stmt, c);
                                    let len = (api.column_bytes)(stmt, c) as usize;
                                    Cell::Blob(if ptr.is_null() {
                                        Vec::new()
                                    } else {
                                        std::slice::from_raw_parts(ptr, len).to_vec()
                                    })
                                }
                                _ => Cell::Null,
                            }
                        };
                        row.push(cell);
                    }
                    result.rows.push(row);
                }
                SQLITE_DONE => break,
                _ => return Err(self.error()),
            }
        }
        // SAFETY: `db` is open.
        unsafe {
            result.affected = if columns == 0 { (api.changes)(self.db) as i64 } else { result.rows.len() as i64 };
            result.last_insert_id = (api.last_insert_rowid)(self.db);
        }
        Ok(result)
    }

    fn in_transaction(&self) -> bool {
        // SAFETY: `db` is open.
        unsafe { (self.api.get_autocommit)(self.db) == 0 }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        // SAFETY: `db` is open and used by no one else.
        unsafe { (self.api.close_v2)(self.db) };
    }
}

// ---- the thread that owns it ------------------------------------------------------------

type Reply = Arc<Answer>;

/// Where the thread puts its answer to a task that is parked waiting for it.
struct Answer {
    done: AtomicBool,
    value: Mutex<Option<Result<Reply2, Error>>>,
    waker: Waker,
}

enum Reply2 {
    Result(QueryResult),
    Number(i64),
    Nothing,
}

enum Job {
    Query { tx: i64, sql: String, args: Vec<Arg>, reply: Reply },
    Script { tx: i64, sql: String, reply: Reply },
    Begin { reply: Reply },
    Finish { tx: i64, commit: bool, reply: Reply },
    Close,
}

impl Job {
    /// The transaction a job belongs to; -1 for none.
    fn tx(&self) -> i64 {
        match self {
            Job::Query { tx, .. } | Job::Script { tx, .. } | Job::Finish { tx, .. } => *tx,
            Job::Begin { .. } | Job::Close => -1,
        }
    }
}

fn answer(reply: &Reply, value: Result<Reply2, Error>) {
    *fiber::lock(&reply.value) = Some(value);
    reply.done.store(true, Ordering::Release);
    reply.waker.wake();
}

static NEXT_TX: Mutex<i64> = Mutex::new(0);

fn serve(connection: Connection, jobs: Receiver<Job>) {
    // The transaction that has the connection, and the work that waits for it to end.
    let mut owner: Option<i64> = None;
    let mut held: VecDeque<Job> = VecDeque::new();
    let mut running = true;
    while running {
        let job = match jobs.recv() {
            Ok(job) => job,
            Err(_) => break,
        };
        let mut queue: VecDeque<Job> = VecDeque::new();
        queue.push_back(job);
        while let Some(job) = queue.pop_front() {
            if matches!(job, Job::Close) {
                running = false;
                // Work that waited for a transaction, or came after the close, gets an answer.
                for waiting in held.drain(..).chain(queue.drain(..)).chain(jobs.try_iter()) {
                    refuse(waiting);
                }
                break;
            }
            let blocked = match &job {
                Job::Begin { .. } => owner.is_some(),
                other => owner.is_some_and(|t| other.tx() != t),
            };
            if blocked {
                held.push_back(job);
                continue;
            }
            match job {
                Job::Query { sql, args, reply, .. } => {
                    answer(&reply, connection.query(&sql, &args).map(Reply2::Result));
                }
                Job::Script { sql, reply, .. } => {
                    answer(&reply, connection.script(&sql).map(|()| Reply2::Nothing));
                }
                Job::Begin { reply } => {
                    let started = connection.script("BEGIN IMMEDIATE");
                    match started {
                        Ok(()) => {
                            let mut next = fiber::lock(&NEXT_TX);
                            *next += 1;
                            owner = Some(*next);
                            answer(&reply, Ok(Reply2::Number(*next)));
                        }
                        Err(error) => answer(&reply, Err(error)),
                    }
                }
                Job::Finish { tx, commit, reply } => {
                    if owner != Some(tx) {
                        // Already ended: `with` rolls back after a commit, which is no error.
                        let outcome = if commit {
                            Err(failure("the transaction has already ended"))
                        } else {
                            Ok(Reply2::Nothing)
                        };
                        answer(&reply, outcome);
                        continue;
                    }
                    let outcome = if commit { connection.script("COMMIT") } else { connection.script("ROLLBACK") };
                    // A failed COMMIT leaves the transaction open: end it, so the connection is free.
                    if outcome.is_err() && connection.in_transaction() {
                        let _ = connection.script("ROLLBACK");
                    }
                    owner = None;
                    answer(&reply, outcome.map(|()| Reply2::Nothing));
                    // What waited for the transaction goes first, in order.
                    let mut waiting = std::mem::take(&mut held);
                    waiting.extend(queue.drain(..));
                    queue = waiting;
                }
                Job::Close => unreachable!(),
            }
        }
    }
    // A transaction still open when the database closes is rolled back by closing it.
    drop(connection);
}

/// Answer a job that will not run.
fn refuse(job: Job) {
    match job {
        Job::Query { reply, .. } | Job::Script { reply, .. } | Job::Begin { reply } | Job::Finish { reply, .. } => {
            answer(&reply, Err(failure("the database was closed")));
        }
        Job::Close => {}
    }
}

struct Database {
    jobs: Mutex<Sender<Job>>,
}

static DATABASES: Mutex<Slab<Arc<Database>>> = Mutex::new(Slab::new());

fn database(id: i64) -> Option<Arc<Database>> {
    fiber::lock(&DATABASES).get(id).cloned()
}

/// Send a job and park until the thread answers, or the task is cancelled.
fn call(id: i64, make: impl FnOnce(Reply) -> Job) -> Result<Reply2, Error> {
    let Some(db) = database(id) else { return Err(failure("not an open database")) };
    let reply: Reply =
        Arc::new(Answer { done: AtomicBool::new(false), value: Mutex::new(None), waker: Waker::current() });
    if fiber::lock(&db.jobs).send(make(reply.clone())).is_err() {
        return Err(failure("the database is closed"));
    }
    while !reply.done.load(Ordering::Acquire) {
        if let Some(reason) = crate::cancel::check() {
            return Err(failure(reason.message().to_string()));
        }
        fiber::park(None);
    }
    fiber::lock(&reply.value).take().unwrap_or_else(|| Err(failure("the database gave no answer")))
}

// ---- the functions compiled code calls ---------------------------------------------------

fn take_args() -> Vec<Arg> {
    ARGS.with(|a| std::mem::take(&mut *a.borrow_mut()))
}

fn number(outcome: Result<Reply2, Error>) -> i64 {
    match outcome {
        Ok(Reply2::Number(n)) => n,
        Ok(_) => 0,
        Err(error) => {
            record(error);
            -1
        }
    }
}

/// Open a database file (or `:memory:`) and start its thread. Returns its id, or -1 after
/// recording the error.
///
/// # Safety
/// `path` and `path_len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_sqlite_open(path: *const u8, path_len: i64) -> i64 {
    let path = unsafe { Str::from_raw(path, path_len) }.to_string();
    // Opened on the calling thread, so a bad path fails here; the connection then moves.
    let (ready, started) = channel::<Result<(), Error>>();
    let (jobs, inbox) = channel::<Job>();
    let spawned = std::thread::Builder::new().name("sqlite".into()).spawn(move || match Connection::open(&path) {
        Ok(connection) => {
            let _ = ready.send(Ok(()));
            serve(connection, inbox);
        }
        Err(error) => {
            let _ = ready.send(Err(error));
        }
    });
    if let Err(error) = spawned {
        record(failure(format!("cannot start the database thread: {error}")));
        return -1;
    }
    // Wait for the open without blocking a worker thread for long: it is a file open.
    match started.recv() {
        Ok(Ok(())) => fiber::lock(&DATABASES).insert(Arc::new(Database { jobs: Mutex::new(jobs) })),
        Ok(Err(error)) => {
            record(error);
            -1
        }
        Err(_) => {
            record(failure("the database thread failed to start"));
            -1
        }
    }
}

/// Add a parameter for the next statement on this thread. `kind`: 0 NULL, 1 integer (`n`), 2 real
/// (`x`), 3 text, 4 blob.
///
/// # Safety
/// `ptr` and `len` describe bytes (UTF-8 for text).
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_sqlite_arg(kind: i64, n: i64, x: f64, ptr: *const u8, len: i64) {
    let arg = match kind {
        1 => Arg::Int(n),
        2 => Arg::Float(x),
        3 => Arg::Text(unsafe { Str::from_raw(ptr, len) }.to_string()),
        4 => Arg::Blob(if len <= 0 {
            Vec::new()
        } else {
            unsafe { std::slice::from_raw_parts(ptr, len as usize) }.to_vec()
        }),
        _ => Arg::Null,
    };
    ARGS.with(|a| a.borrow_mut().push(arg));
}

/// Run a statement with the parameters added since the last call, on the database, or inside the
/// transaction `tx` (-1 for none). Returns a result handle, or -1 after recording the error.
///
/// # Safety
/// `sql` and `sql_len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_sqlite_query(db: i64, tx: i64, sql: *const u8, sql_len: i64) -> i64 {
    let sql = unsafe { Str::from_raw(sql, sql_len) }.to_string();
    let args = take_args();
    match call(db, |reply| Job::Query { tx, sql, args, reply }) {
        Ok(Reply2::Result(result)) => fiber::lock(&RESULTS).insert(result),
        Ok(_) => -1,
        Err(error) => {
            record(error);
            -1
        }
    }
}

/// Run several statements without parameters. Returns 0, or -1 after recording the error.
///
/// # Safety
/// `sql` and `sql_len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_sqlite_script(db: i64, tx: i64, sql: *const u8, sql_len: i64) -> i64 {
    let sql = unsafe { Str::from_raw(sql, sql_len) }.to_string();
    take_args();
    number(call(db, |reply| Job::Script { tx, sql, reply }))
}

/// Start a transaction (`BEGIN IMMEDIATE`), which holds the connection until it ends. Returns its
/// id, or -1.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_sqlite_begin(db: i64) -> i64 {
    number(call(db, |reply| Job::Begin { reply }))
}

/// Commit or roll back a transaction. Returns 0, or -1 after recording the error; either way the
/// transaction is over.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_sqlite_finish(db: i64, tx: i64, commit: i64) -> i64 {
    number(call(db, |reply| Job::Finish { tx, commit: commit != 0, reply }))
}

fn with_result<T>(handle: i64, default: T, f: impl FnOnce(&QueryResult) -> T) -> T {
    fiber::lock(&RESULTS).get(handle).map_or(default, f)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_sqlite_rows(handle: i64) -> i64 {
    with_result(handle, 0, |r| r.rows.len() as i64)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_sqlite_columns(handle: i64) -> i64 {
    with_result(handle, 0, |r| r.columns.len() as i64)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_sqlite_column(handle: i64, column: i64) -> Str {
    with_result(handle, Str::empty(), |r| {
        r.columns.get(column as usize).map_or(Str::empty(), |name| Str::from_bytes(name.as_bytes()))
    })
}

/// A cell's kind: 0 NULL, 1 integer, 2 real, 3 text, 4 blob; and a boolean column's integers are
/// kind 5.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_sqlite_kind(handle: i64, row: i64, column: i64) -> i64 {
    with_result(handle, 0, |r| match r.rows.get(row as usize).and_then(|row| row.get(column as usize)) {
        Some(Cell::Int(_)) if r.declared[column as usize].contains("BOOL") => 5,
        Some(Cell::Int(_)) => 1,
        Some(Cell::Float(_)) => 2,
        Some(Cell::Text(_)) => 3,
        Some(Cell::Blob(_)) => 4,
        _ => 0,
    })
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_sqlite_int(handle: i64, row: i64, column: i64) -> i64 {
    with_result(handle, 0, |r| match r.rows.get(row as usize).and_then(|row| row.get(column as usize)) {
        Some(Cell::Int(i)) => *i,
        _ => 0,
    })
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_sqlite_float(handle: i64, row: i64, column: i64) -> f64 {
    with_result(handle, 0.0, |r| match r.rows.get(row as usize).and_then(|row| row.get(column as usize)) {
        Some(Cell::Float(f)) => *f,
        Some(Cell::Int(i)) => *i as f64,
        _ => 0.0,
    })
}

/// A text or blob cell's bytes.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_sqlite_bytes(handle: i64, row: i64, column: i64) -> Str {
    with_result(handle, Str::empty(), |r| match r.rows.get(row as usize).and_then(|row| row.get(column as usize)) {
        Some(Cell::Text(bytes) | Cell::Blob(bytes)) => Str::from_bytes(bytes),
        _ => Str::empty(),
    })
}

/// Rows changed by the statement, or returned by it.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_sqlite_affected(handle: i64) -> i64 {
    with_result(handle, 0, |r| r.affected)
}

/// The rowid of the last row inserted on the connection, as of the statement.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_sqlite_last_id(handle: i64) -> i64 {
    with_result(handle, 0, |r| r.last_insert_id)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_sqlite_json(handle: i64) -> Str {
    with_result(handle, Str::empty(), |r| Str::from_bytes(&r.to_json()))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_sqlite_free(handle: i64) {
    fiber::lock(&RESULTS).remove(handle);
}

/// The name of the last error's code on this thread (`SQLITE_CONSTRAINT_UNIQUE`).
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_sqlite_error_code() -> Str {
    LAST_CODE.with(|c| Str::from_bytes(c.borrow().as_bytes()))
}

/// Close a database: its thread finishes the work already sent, then closes the connection.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_sqlite_close(db: i64) {
    if let Some(database) = fiber::lock(&DATABASES).remove(db) {
        let _ = fiber::lock(&database.jobs).send(Job::Close);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_names() {
        assert_eq!(code_name(2067), "SQLITE_CONSTRAINT_UNIQUE");
        assert_eq!(code_name(19), "SQLITE_CONSTRAINT");
        assert_eq!(code_name(1 | (4 << 8)), "SQLITE_ERROR");
        assert_eq!(code_name(5), "SQLITE_BUSY");
        assert_eq!(code_name(14), "SQLITE_CANTOPEN");
    }

    /// With a real libsqlite3: types round-trip, parameters bind, and errors have names.
    #[test]
    fn a_real_database() {
        if api().is_err() {
            return;
        }
        let db = Connection::open(":memory:").unwrap();
        db.script("create table t (id integer primary key, name text unique, score real, ok boolean, data blob)")
            .unwrap();
        let args = [Arg::Int(1), Arg::Text("ann".into()), Arg::Float(2.5), Arg::Int(1), Arg::Blob(vec![0, 255])];
        let inserted = db.query("insert into t values (?, ?, ?, ?, ?)", &args).unwrap();
        assert_eq!((inserted.affected, inserted.last_insert_id), (1, 1));
        let found = db.query("select * from t where name = ?", &[Arg::Text("ann".into())]).unwrap();
        assert_eq!(found.rows.len(), 1);
        assert_eq!(
            String::from_utf8(found.to_json()).unwrap(),
            r#"[{"id":1,"name":"ann","score":2.5,"ok":true,"data":"AP8="}]"#
        );
        let again = db.query("insert into t values (2, ?, 0, 0, null)", &[Arg::Text("ann".into())]).unwrap_err();
        assert_eq!(again.code, "SQLITE_CONSTRAINT_UNIQUE");
        let wrong = db.query("select ?", &[]).unwrap_err();
        assert!(wrong.message.contains("1 parameter"), "{}", wrong.message);
        assert_eq!(db.query("selec 1", &[]).unwrap_err().code, "SQLITE_ERROR");
    }

    #[test]
    fn rows_as_json() {
        let result = QueryResult {
            columns: vec!["id".into(), "ok".into(), "name".into(), "score".into(), "data".into(), "gone".into()],
            declared: vec![
                "INTEGER".into(),
                "BOOLEAN".into(),
                "TEXT".into(),
                "REAL".into(),
                "BLOB".into(),
                String::new(),
            ],
            rows: vec![vec![
                Cell::Int(7),
                Cell::Int(1),
                Cell::Text(b"a \"b\"".to_vec()),
                Cell::Float(2.5),
                Cell::Blob(vec![1, 2, 3]),
                Cell::Null,
            ]],
            ..Default::default()
        };
        assert_eq!(
            String::from_utf8(result.to_json()).unwrap(),
            r#"[{"id":7,"ok":true,"name":"a \"b\"","score":2.5,"data":"AQID","gone":null}]"#
        );
    }
}
