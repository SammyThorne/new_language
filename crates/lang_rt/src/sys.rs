//! Files, standard streams, program arguments and the environment: the runtime half of the
//! standard library's `io` and `os` modules (in `stdlib/` at the repository root).
//!
//! The modules bind these with `@extern`. Errors cannot be returned as a `Result` across the C
//! boundary, so a failing call returns a placeholder value and records a message, which
//! `lrt_io_error` then returns (and clears). The message is per thread.
//!
//! Open files live in a table indexed by small integers, the handles that `io.File` holds. The
//! table holds Rust values only, never references to collected objects.

use crate::{Str, out};
use std::cell::RefCell;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, Ordering};

thread_local! {
    static LAST_ERROR: RefCell<String> = const { RefCell::new(String::new()) };
}

fn fail(error: impl std::fmt::Display) {
    LAST_ERROR.with(|e| *e.borrow_mut() = error.to_string());
}

/// Record an error for `lrt_io_error`, from other parts of the runtime.
pub(crate) fn record_error(error: impl std::fmt::Display) {
    fail(error);
}

/// A message describing an I/O error, without Rust's "(os error N)" suffix.
fn describe(error: std::io::Error) -> String {
    let text = error.to_string();
    match text.find(" (os error") {
        Some(at) => text[..at].to_string(),
        None => text,
    }
}

/// # Safety
/// `ptr` and `len` must describe a UTF-8 string.
unsafe fn text<'a>(ptr: *const u8, len: i64) -> &'a str {
    unsafe { Str::from_raw(ptr, len) }
}

/// The message of the last failed call on this thread, or `""`, and clear it.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_io_error() -> Str {
    let message = LAST_ERROR.with(|e| std::mem::take(&mut *e.borrow_mut()));
    Str::from_bytes(message.as_bytes())
}

/// # Safety
/// The arguments must describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_io_read_file(path: *const u8, path_len: i64) -> Str {
    let path = unsafe { text(path, path_len) };
    match std::fs::read(path) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => Str::from_bytes(text.as_bytes()),
            Err(_) => {
                fail("the file is not valid UTF-8 text");
                Str::empty()
            }
        },
        Err(error) => {
            fail(describe(error));
            Str::empty()
        }
    }
}

/// Write (or with `append`, add to) a file. Returns 1 on success.
///
/// # Safety
/// The arguments must describe UTF-8 strings.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_io_write_file(
    path: *const u8,
    path_len: i64,
    data: *const u8,
    data_len: i64,
    append: i64,
) -> i64 {
    let (path, data) = unsafe { (text(path, path_len), text(data, data_len)) };
    let file = OpenOptions::new().create(true).write(true).append(append != 0).truncate(append == 0).open(path);
    match file.and_then(|mut f| f.write_all(data.as_bytes())) {
        Ok(()) => 1,
        Err(error) => {
            fail(describe(error));
            0
        }
    }
}

/// # Safety
/// The arguments must describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_io_exists(path: *const u8, path_len: i64) -> i64 {
    i64::from(std::path::Path::new(unsafe { text(path, path_len) }).exists())
}

/// Whether the path is a directory: 1 or 0.
///
/// # Safety
/// The arguments must describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_io_is_dir(path: *const u8, path_len: i64) -> i64 {
    i64::from(std::path::Path::new(unsafe { text(path, path_len) }).is_dir())
}

/// When a file was last modified, in nanoseconds since the Unix epoch; -1 if it cannot be read.
///
/// # Safety
/// The arguments must describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_io_mtime(path: *const u8, path_len: i64) -> i64 {
    std::fs::metadata(unsafe { text(path, path_len) })
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(-1, |d| d.as_nanos() as i64)
}

/// Remove a file. Returns 1 on success.
///
/// # Safety
/// The arguments must describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_io_remove(path: *const u8, path_len: i64) -> i64 {
    match std::fs::remove_file(unsafe { text(path, path_len) }) {
        Ok(()) => 1,
        Err(error) => {
            fail(describe(error));
            0
        }
    }
}

thread_local! {
    static LISTING: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// Read a directory's entry names, sorted, for `lrt_io_list_entry`. Returns how many there are,
/// or -1 on error.
///
/// # Safety
/// The arguments must describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_io_list_dir(path: *const u8, path_len: i64) -> i64 {
    let entries = std::fs::read_dir(unsafe { text(path, path_len) }).and_then(|dir| {
        dir.map(|entry| entry.map(|e| e.file_name().to_string_lossy().into_owned())).collect::<Result<Vec<_>, _>>()
    });
    match entries {
        Ok(mut names) => {
            names.sort();
            let count = names.len() as i64;
            LISTING.with(|l| *l.borrow_mut() = names);
            count
        }
        Err(error) => {
            fail(describe(error));
            -1
        }
    }
}

/// Entry `index` of the last directory listed on this thread.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_io_list_entry(index: i64) -> Str {
    LISTING.with(|l| l.borrow().get(index as usize).map_or_else(Str::empty, |name| Str::from_bytes(name.as_bytes())))
}

/// A line from standard input without its line ending, or `None` (a null string) at the end.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_io_read_line() -> Str {
    out::flush();
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) | Err(_) => Str { ptr: std::ptr::null(), len: 0 },
        Ok(_) => {
            let trimmed = line.strip_suffix('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).unwrap_or(&line);
            Str::from_bytes(trimmed.as_bytes())
        }
    }
}

/// Write a line to standard error, after flushing standard output so the two stay in order.
///
/// # Safety
/// The arguments must describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_io_eprint(data: *const u8, len: i64) {
    out::flush();
    let data = unsafe { text(data, len) };
    let _ = writeln!(std::io::stderr().lock(), "{data}");
}

// ---- open files -------------------------------------------------------------------------

enum OpenFile {
    Read(BufReader<File>),
    Write(File),
}

static FILES: Mutex<Vec<Option<OpenFile>>> = Mutex::new(Vec::new());

fn with_file<R>(handle: i64, body: impl FnOnce(&mut OpenFile) -> R) -> Option<R> {
    let mut files = FILES.lock().unwrap_or_else(|p| p.into_inner());
    match files.get_mut(handle as usize).and_then(Option::as_mut) {
        Some(file) => Some(body(file)),
        None => {
            fail("the file is closed");
            None
        }
    }
}

/// Open a file with mode `r`, `w` or `a`. Returns a handle, or -1 on error.
///
/// # Safety
/// The arguments must describe UTF-8 strings.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_file_open(path: *const u8, path_len: i64, mode: *const u8, mode_len: i64) -> i64 {
    let (path, mode) = unsafe { (text(path, path_len), text(mode, mode_len)) };
    let opened = match mode {
        "r" => File::open(path).map(|f| OpenFile::Read(BufReader::new(f))),
        "w" => File::create(path).map(OpenFile::Write),
        "a" => OpenOptions::new().create(true).append(true).open(path).map(OpenFile::Write),
        other => {
            fail(format!("unknown mode \"{other}\"; use \"r\", \"w\" or \"a\""));
            return -1;
        }
    };
    match opened {
        Ok(file) => {
            let mut files = FILES.lock().unwrap_or_else(|p| p.into_inner());
            match files.iter().position(Option::is_none) {
                Some(free) => {
                    files[free] = Some(file);
                    free as i64
                }
                None => {
                    files.push(Some(file));
                    files.len() as i64 - 1
                }
            }
        }
        Err(error) => {
            fail(describe(error));
            -1
        }
    }
}

/// The rest of the file. On error, records it and returns `""`.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_file_read(handle: i64) -> Str {
    let result = with_file(handle, |file| match file {
        OpenFile::Read(reader) => {
            let mut text = String::new();
            reader.read_to_string(&mut text).map(|_| text).map_err(describe)
        }
        OpenFile::Write(_) => Err("the file was opened for writing".to_string()),
    });
    match result {
        Some(Ok(text)) => Str::from_bytes(text.as_bytes()),
        Some(Err(message)) => {
            fail(message);
            Str::empty()
        }
        None => Str::empty(),
    }
}

/// The next line without its line ending, or `None` (a null string) at the end of the file.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_file_read_line(handle: i64) -> Str {
    let none = Str { ptr: std::ptr::null(), len: 0 };
    let line = with_file(handle, |file| match file {
        OpenFile::Read(reader) => {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => None,
                Ok(_) => Some(line),
            }
        }
        OpenFile::Write(_) => None,
    });
    match line.flatten() {
        Some(line) => {
            let trimmed = line.strip_suffix('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).unwrap_or(&line);
            Str::from_bytes(trimmed.as_bytes())
        }
        None => none,
    }
}

/// Write text to a file opened for writing. Returns 1 on success.
///
/// # Safety
/// The arguments must describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_file_write(handle: i64, data: *const u8, len: i64) -> i64 {
    let data = unsafe { text(data, len) };
    let result = with_file(handle, |file| match file {
        OpenFile::Write(f) => f.write_all(data.as_bytes()).map_err(describe),
        OpenFile::Read(_) => Err("the file was opened for reading".to_string()),
    });
    match result {
        Some(Ok(())) => 1,
        Some(Err(message)) => {
            fail(message);
            0
        }
        None => 0,
    }
}

/// Close a file. Closing twice does nothing.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_file_close(handle: i64) {
    let mut files = FILES.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(slot) = files.get_mut(handle as usize) {
        *slot = None;
    }
}

// ---- the process ------------------------------------------------------------------------

static ARGV: AtomicPtr<*const std::ffi::c_char> = AtomicPtr::new(std::ptr::null_mut());
static ARGC: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// Called once from `main` with the process's arguments.
pub fn set_args(argc: i64, argv: *const *const std::ffi::c_char) {
    ARGC.store(argc, Ordering::Relaxed);
    ARGV.store(argv as *mut _, Ordering::Relaxed);
}

/// The number of command-line arguments, including the program's name.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_os_arg_count() -> i64 {
    ARGC.load(Ordering::Relaxed)
}

/// Argument `index`, or `""` if there is none.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_os_arg(index: i64) -> Str {
    let argv = ARGV.load(Ordering::Relaxed);
    if argv.is_null() || index < 0 || index >= ARGC.load(Ordering::Relaxed) {
        return Str::empty();
    }
    // SAFETY: `argv` holds `argc` valid C strings for the life of the process.
    let arg = unsafe { std::ffi::CStr::from_ptr(*argv.add(index as usize)) };
    Str::from_bytes(arg.to_string_lossy().as_bytes())
}

/// An environment variable, or `None` (a null string) if it is not set.
///
/// # Safety
/// The arguments must describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_os_getenv(name: *const u8, len: i64) -> Str {
    match std::env::var(unsafe { text(name, len) }) {
        Ok(value) => Str::from_bytes(value.as_bytes()),
        Err(_) => Str { ptr: std::ptr::null(), len: 0 },
    }
}

/// Every environment variable as `NAME=value`, each followed by a NUL (which neither can
/// hold). Variables whose name or value is not UTF-8 are left out.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_os_environ() -> Str {
    let mut out = String::new();
    for (name, value) in std::env::vars_os() {
        if let (Some(name), Some(value)) = (name.to_str(), value.to_str()) {
            out.push_str(name);
            out.push('=');
            out.push_str(value);
            out.push('\0');
        }
    }
    Str::from_bytes(out.as_bytes())
}

/// Set an environment variable. Returns 0, or -1 if the name is empty or holds `=` or NUL, or
/// the value holds NUL, which the environment cannot store.
///
/// # Safety
/// The arguments must describe UTF-8 strings.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_os_setenv(name: *const u8, name_len: i64, value: *const u8, value_len: i64) -> i64 {
    let (name, value) = unsafe { (text(name, name_len), text(value, value_len)) };
    if name.is_empty() || name.contains(['=', '\0']) || value.contains('\0') {
        return -1;
    }
    // SAFETY: the C library's environment is not thread-safe; the documentation of
    // `os.setenv` says to set variables at startup, before starting tasks or servers.
    unsafe { std::env::set_var(name, value) };
    0
}

/// End the program with an exit status, after flushing standard output.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_os_exit(code: i64) {
    crate::trace::at_exit();
    out::flush();
    std::process::exit(code as i32)
}

/// The current working directory.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_os_cwd() -> Str {
    let dir = std::env::current_dir().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
    Str::from_bytes(dir.as_bytes())
}
