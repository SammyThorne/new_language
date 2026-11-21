//! Logging to standard error (`log` in the standard library): levels, fields, a text or JSON
//! format, and the request ID of the task writing the line (docs/14, milestone 1).
//!
//! While a server runs and standard error is not a terminal, lines are buffered and written in
//! blocks: when 16 KiB have gathered, at once for `warn` and `error`, when the server's accept
//! loop next wakes (at most every 200 ms), and when the program exits or panics. A server that
//! logs every request would otherwise make a system call per request.

use crate::{Str, fiber};
use std::cell::Cell;
use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

static LOG_LEVEL: AtomicI64 = AtomicI64::new(-1);
/// 0 for text, 1 for JSON; -1 until read from the environment.
static LOG_FORMAT: AtomicI64 = AtomicI64::new(-1);
/// How many servers are running with buffered logs.
static BUFFERING: AtomicI64 = AtomicI64::new(0);
static BUFFER: Mutex<Vec<u8>> = Mutex::new(Vec::new());
const FLUSH_AT: usize = 16 * 1024;

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

fn json_format() -> bool {
    let format = LOG_FORMAT.load(Ordering::Relaxed);
    if format >= 0 {
        return format == 1;
    }
    let json = std::env::var(lang_names::env_var("LOG_FORMAT")).is_ok_and(|v| v.trim().eq_ignore_ascii_case("json"));
    LOG_FORMAT.store(json as i64, Ordering::Relaxed);
    json
}

// ---- the request context ----------------------------------------------------------------

/// The longest request ID kept; `http.request_log` accepts no longer one from a client.
pub(crate) const MAX_REQUEST_ID: usize = 128;

/// What a task carries for its log lines: the ID of the request it is handling, if any. It is a
/// counted string, not a collected one, so that a thread outside the fiber runtime can hold it
/// in a thread-local, which the collector does not scan; and one word plus a length, so that a
/// task without a request costs nothing more. Tasks start with (a reference to) their parent's.
pub(crate) struct Context(Cell<Option<Arc<str>>>);

impl Context {
    /// The running task's context, for a task it spawns.
    pub(crate) fn inherited() -> Context {
        Context(Cell::new(with_current(Context::request_id)))
    }

    fn request_id(&self) -> Option<Arc<str>> {
        let id = self.0.take();
        self.0.set(id.clone());
        id
    }
}

thread_local! {
    static THREAD_CONTEXT: Context = const { Context(Cell::new(None)) };
}

fn with_current<R>(f: impl FnOnce(&Context) -> R) -> R {
    match fiber::current() {
        Some(me) => f(&me.context),
        None => THREAD_CONTEXT.with(f),
    }
}

/// Forget the running task's request ID: after a handler that panicked, so the connection's
/// next request does not log with it.
pub(crate) fn clear_request_id() {
    with_current(|c| c.0.set(None));
}

/// Set the running task's request ID (`""` to clear it). Longer IDs are cut to
/// [`MAX_REQUEST_ID`] bytes, at a character boundary.
///
/// # Safety
/// `ptr` and `len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_log_set_request_id(ptr: *const u8, len: i64) {
    let id = unsafe { Str::from_raw(ptr, len) };
    let mut end = id.len().min(MAX_REQUEST_ID);
    while !id.is_char_boundary(end) {
        end -= 1;
    }
    let id = (end > 0).then(|| Arc::from(&id[..end]));
    with_current(|c| c.0.set(id));
}

/// The running task's request ID, or `""`.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_log_request_id() -> Str {
    Str::from_bytes(with_current(Context::request_id).as_deref().unwrap_or("").as_bytes())
}

/// A new request ID: 16 hexadecimal digits, different for every call in this process and
/// unpredictable across processes. A counter scrambled by a bijection, so making one takes no
/// system call; `uuid.v7()` replaces it in docs/14's milestone 6.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_log_new_request_id() -> Str {
    Str::from_bytes(new_request_id().as_bytes())
}

fn new_request_id() -> String {
    static SEED: OnceLock<u64> = OnceLock::new();
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seed = *SEED.get_or_init(|| {
        let mut bytes = [0u8; 8];
        let _ = ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes);
        u64::from_le_bytes(bytes)
    });
    // The finaliser of SplitMix64: each step is invertible, so distinct inputs stay distinct.
    let mut z = seed ^ COUNTER.fetch_add(1, Ordering::Relaxed);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^= z >> 31;
    format!("{z:016x}")
}

// ---- writing ----------------------------------------------------------------------------

/// Set the lowest level that is written: `debug`, `info`, `warn`, `error` or `off`.
///
/// # Safety
/// `ptr` and `len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_log_set_level(ptr: *const u8, len: i64) {
    LOG_LEVEL.store(level_number(unsafe { Str::from_raw(ptr, len) }), Ordering::Relaxed);
}

/// Set the format: `json`, or anything else for text.
///
/// # Safety
/// `ptr` and `len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_log_set_format(ptr: *const u8, len: i64) {
    let json = unsafe { Str::from_raw(ptr, len) } == "json";
    LOG_FORMAT.store(json as i64, Ordering::Relaxed);
}

/// 1 if lines of this level are written.
///
/// # Safety
/// `ptr` and `len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_log_enabled(ptr: *const u8, len: i64) -> i64 {
    (level_number(unsafe { Str::from_raw(ptr, len) }) >= log_level()) as i64
}

/// Write `2026-11-18T12:00:00.123Z INFO  message` to standard error, if the level is enabled.
///
/// # Safety
/// The arguments describe UTF-8 strings.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_log_write(level: *const u8, level_len: i64, message: *const u8, message_len: i64) {
    let (level, message) = unsafe { (Str::from_raw(level, level_len), Str::from_raw(message, message_len)) };
    write_line(level, message, "");
}

/// [`lrt_log_write`] with fields: names and values, each followed by a NUL character.
///
/// # Safety
/// The arguments describe UTF-8 strings.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_log_write_fields(
    level: *const u8,
    level_len: i64,
    message: *const u8,
    message_len: i64,
    fields: *const u8,
    fields_len: i64,
) {
    let (level, message) = unsafe { (Str::from_raw(level, level_len), Str::from_raw(message, message_len)) };
    write_line(level, message, unsafe { Str::from_raw(fields, fields_len) });
}

fn write_line(level: &str, message: &str, fields: &str) {
    let number = level_number(level);
    if number < log_level() {
        return;
    }
    let id = with_current(Context::request_id);
    let line = format_line(level, message, id.as_deref().unwrap_or(""), fields, json_format(), now());
    emit(&line, number >= 2);
}

/// Milliseconds since the Unix epoch.
fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn timestamp(millis: u64) -> String {
    let seconds = millis / 1000;
    let (year, month, day) = crate::http::civil((seconds / 86_400) as i64);
    let rest = seconds % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3600,
        rest / 60 % 60,
        rest % 60,
        millis % 1000
    )
}

/// The pairs in a fields string; a name without a value is left out.
fn pairs(fields: &str) -> impl Iterator<Item = (&str, &str)> {
    let mut parts = fields.split('\0');
    std::iter::from_fn(move || {
        let name = parts.next()?;
        let value = parts.next()?;
        Some((name, value))
    })
}

fn format_line(level: &str, message: &str, request_id: &str, fields: &str, json: bool, millis: u64) -> Vec<u8> {
    let mut line = String::with_capacity(48 + message.len() + fields.len() + request_id.len());
    if json {
        line.push_str("{\"time\":\"");
        line.push_str(&timestamp(millis));
        line.push_str("\",\"level\":");
        push_json_string(&mut line, &level.to_ascii_lowercase());
        line.push_str(",\"msg\":");
        push_json_string(&mut line, message);
        if !request_id.is_empty() {
            line.push_str(",\"request_id\":");
            push_json_string(&mut line, request_id);
        }
        for (name, value) in pairs(fields) {
            line.push(',');
            // The line's own keys win; a field with one of their names keeps it with a `_`.
            if matches!(name, "time" | "level" | "msg" | "request_id") {
                push_json_string(&mut line, &format!("_{name}"));
            } else {
                push_json_string(&mut line, name);
            }
            line.push(':');
            push_json_string(&mut line, value);
        }
        line.push_str("}\n");
    } else {
        line.push_str(&timestamp(millis));
        line.push(' ');
        line.push_str(&format!("{:<5}", level.to_ascii_uppercase()));
        line.push(' ');
        line.push_str(message);
        if !request_id.is_empty() {
            push_text_field(&mut line, "request_id", request_id);
        }
        for (name, value) in pairs(fields) {
            push_text_field(&mut line, name, value);
        }
        line.push('\n');
    }
    line.into_bytes()
}

fn push_json_string(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// ` name=value`, as logfmt writes it: the value in double quotes, with JSON's escapes, when
/// it is empty or has a space, `=`, `"` or a control character. Such characters in the name
/// become `_`.
fn push_text_field(out: &mut String, name: &str, value: &str) {
    out.push(' ');
    if name.is_empty() {
        out.push('_');
    }
    for c in name.chars() {
        out.push(if c.is_whitespace() || c.is_control() || c == '=' || c == '"' { '_' } else { c });
    }
    out.push('=');
    if value.is_empty() || value.chars().any(|c| c.is_whitespace() || c.is_control() || c == '=' || c == '"') {
        push_json_string(out, value);
    } else {
        out.push_str(value);
    }
}

fn emit(line: &[u8], urgent: bool) {
    // Output printed before the line comes before it, when both go to one terminal or file.
    crate::out::flush_stdout();
    if BUFFERING.load(Ordering::Relaxed) > 0 {
        let mut buffer = BUFFER.lock().unwrap_or_else(|e| e.into_inner());
        buffer.extend_from_slice(line);
        if urgent || buffer.len() >= FLUSH_AT {
            let _ = std::io::stderr().lock().write_all(&buffer);
            buffer.clear();
        }
        return;
    }
    let _ = std::io::stderr().lock().write_all(line);
}

/// Buffer log lines while a server runs, unless standard error is a terminal, where each line
/// should appear as it is written.
pub(crate) fn start_buffering() {
    if !std::io::stderr().is_terminal() {
        BUFFERING.fetch_add(1, Ordering::SeqCst);
    }
}

pub(crate) fn stop_buffering() {
    if !std::io::stderr().is_terminal() {
        BUFFERING.fetch_sub(1, Ordering::SeqCst);
    }
    flush();
}

/// Write out buffered lines.
pub(crate) fn flush() {
    let mut buffer = BUFFER.lock().unwrap_or_else(|e| e.into_inner());
    if !buffer.is_empty() {
        let _ = std::io::stderr().lock().write_all(&buffer);
        buffer.clear();
    }
}

/// [`flush`], unless another thread is writing a line at this moment: for a signal handler,
/// which must not wait for a lock.
pub(crate) fn try_flush() {
    if let Ok(mut buffer) = BUFFER.try_lock()
        && !buffer.is_empty()
    {
        let _ = std::io::stderr().lock().write_all(&buffer);
        buffer.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(json: bool, request_id: &str, fields: &str) -> String {
        String::from_utf8(format_line("info", "saved \"a\"", request_id, fields, json, 1_795_000_000_123)).unwrap()
    }

    #[test]
    fn text_lines_keep_the_old_form_and_add_fields_as_logfmt() {
        assert_eq!(line(false, "", ""), "2026-11-18T11:06:40.123Z INFO  saved \"a\"\n");
        assert_eq!(
            line(false, "ab12", "user\x0042\0note\0two words\0empty\0\0bad key\0x=y\0"),
            "2026-11-18T11:06:40.123Z INFO  saved \"a\" request_id=ab12 user=42 note=\"two words\" empty=\"\" bad_key=\"x=y\"\n"
        );
    }

    #[test]
    fn json_lines_escape_and_keep_their_own_keys() {
        assert_eq!(
            line(true, "ab12", "user\x0042\0msg\0line\nbreak\0"),
            "{\"time\":\"2026-11-18T11:06:40.123Z\",\"level\":\"info\",\"msg\":\"saved \\\"a\\\"\",\"request_id\":\"ab12\",\"user\":\"42\",\"_msg\":\"line\\nbreak\"}\n"
        );
    }

    #[test]
    fn request_ids_differ() {
        let ids: std::collections::HashSet<String> = (0..1000).map(|_| new_request_id()).collect();
        assert_eq!(ids.len(), 1000);
        assert!(ids.iter().all(|id| id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit())));
    }
}
