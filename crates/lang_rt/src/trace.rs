//! Traces (docs/14, milestone 10): spans for the requests a server answers, the requests it
//! sends (`http.fetch`), its PostgreSQL queries and the program's own `with trace.span(...)`
//! blocks, sent in batches to an OpenTelemetry collector as OTLP/JSON over HTTP.
//!
//! **Off costs one load.** Until `trace.init` is given an endpoint, every entry point checks
//! [`on`] (a relaxed load) and returns.
//!
//! **The context** is the running task's: the trace ID, the current span's ID, whether the trace
//! is sampled, and the caller's `tracestate`. It lives next to the request ID in the task's log
//! context (`log.rs`), so tasks a handler spawns continue the trace, and log lines carry
//! `trace_id` and `span_id`. Starting a span makes it the current one; ending it puts back its
//! parent.
//!
//! **Sampling** follows the parent when there is one (an incoming `traceparent`'s flag, or the
//! enclosing span); a new trace is sampled when the low 64 bits of its random ID fall under the
//! ratio. An unsampled span still has IDs, so `traceparent` goes out with the `00` flag and the
//! services after this one agree; it is just not recorded.
//!
//! **Export.** Ended spans go into a queue of at most [`MAX_QUEUE`]; beyond that they are dropped
//! and counted. A thread, `trace-export`, sends a batch of up to [`BATCH`] when that many are
//! waiting or every [`INTERVAL`], POSTing `{"resourceSpans": ...}` to `<endpoint>/v1/traces`.
//! A batch the collector does not take (it is down, or answers with an error) is dropped and
//! counted too, with one warning until an export succeeds again. [`shutdown`] sends what is left
//! when the program ends.

use crate::Str;
use crate::log::TraceContext;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Spans waiting at most; more are dropped.
pub(crate) const MAX_QUEUE: usize = 2048;
/// Spans sent in one request at most, and the number that wakes the exporter early.
pub(crate) const BATCH: usize = 512;
/// How often waiting spans are sent.
const INTERVAL: Duration = Duration::from_secs(5);
/// How long one export may take.
const EXPORT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long [`shutdown`] waits for the collector.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
/// The longest `tracestate` passed on, as the W3C recommendation allows.
const MAX_STATE: usize = 512;

static ON: AtomicBool = AtomicBool::new(false);
static CONFIG: OnceLock<Config> = OnceLock::new();
static QUEUE: Mutex<Vec<Span>> = Mutex::new(Vec::new());
static WAKE: Condvar = Condvar::new();
static DROPPED: AtomicU64 = AtomicU64::new(0);
static EXPORTED: AtomicU64 = AtomicU64::new(0);
/// Set after a failed export, so that only the first of a run of failures is logged.
static FAILING: AtomicBool = AtomicBool::new(false);

struct Config {
    service: String,
    /// The full URL spans are POSTed to.
    url: String,
    /// The sampling ratio as a threshold on the trace ID's low 64 bits.
    threshold: u64,
    /// Exporting is serialised, so shutdown and the thread never send the same spans twice
    /// or race for the queue's order.
    exporting: Mutex<()>,
    interval: Duration,
}

/// Whether tracing is on.
#[inline]
pub(crate) fn on() -> bool {
    ON.load(Ordering::Relaxed)
}

/// Spans dropped since the start: the queue was full, or the collector did not take them.
pub(crate) fn dropped() -> u64 {
    DROPPED.load(Ordering::Relaxed)
}

/// Spans the collector has taken.
pub(crate) fn exported() -> u64 {
    EXPORTED.load(Ordering::Relaxed)
}

// ---- identifiers ----------------------------------------------------------------------------

thread_local! {
    static RANDOM: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// A random 64-bit number: splitmix64 from a per-thread seed taken from the system's generator.
fn random() -> u64 {
    RANDOM.with(|state| {
        let mut s = state.get();
        if s == 0 {
            let mut seed = [0u8; 8];
            let _ = ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut seed);
            s = u64::from_le_bytes(seed) | 1;
        }
        s = s.wrapping_add(0x9e37_79b9_7f4a_7c15);
        state.set(s);
        let mut z = s;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    })
}

fn nonzero_random() -> u64 {
    loop {
        let n = random();
        if n != 0 {
            return n;
        }
    }
}

fn unix_nanos() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64)
}

fn hex(value: u128, digits: usize) -> String {
    format!("{value:0digits$x}")
}

/// `traceparent` for a context: `00-<trace>-<span>-<flags>`.
pub(crate) fn traceparent(context: &TraceContext) -> String {
    format!(
        "00-{}-{}-{}",
        hex(context.trace_id, 32),
        hex(u128::from(context.span_id), 16),
        if context.sampled { "01" } else { "00" }
    )
}

/// The context in a `traceparent` header (and its `tracestate`), if the header is well formed:
/// version `00` with exactly four fields, or a later version with at least four (as the
/// specification asks, the rest is ignored); IDs that are not all zeros; lower-case hex.
pub(crate) fn parse_traceparent(header: &str, state: Option<&str>) -> Option<TraceContext> {
    let header = header.trim();
    let mut parts = header.split('-');
    let version = parts.next()?;
    let (trace, span, flags) = (parts.next()?, parts.next()?, parts.next()?);
    let lower_hex =
        |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !lower_hex(version, 2) || version == "ff" || (version == "00" && parts.next().is_some()) {
        return None;
    }
    if !lower_hex(trace, 32) || !lower_hex(span, 16) || !lower_hex(flags, 2) {
        return None;
    }
    let trace_id = u128::from_str_radix(trace, 16).ok()?;
    let span_id = u64::from_str_radix(span, 16).ok()?;
    if trace_id == 0 || span_id == 0 {
        return None;
    }
    let sampled = u8::from_str_radix(flags, 16).ok()? & 1 == 1;
    let state = state.map(str::trim).filter(|s| !s.is_empty() && s.len() <= MAX_STATE).map(Arc::from);
    Some(TraceContext { trace_id, span_id, sampled, state })
}

// ---- spans ----------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    Internal = 1,
    Server = 2,
    Client = 3,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Value {
    Text(String),
    Int(i64),
}

#[derive(Debug)]
pub(crate) struct Span {
    trace_id: u128,
    span_id: u64,
    parent_id: u64,
    name: String,
    kind: Kind,
    start: u64,
    end: u64,
    attributes: Vec<(String, Value)>,
    /// `Some` for a span that failed, with its message.
    error: Option<String>,
}

/// A span that has started: what it will record, and the context to put back when it ends.
pub(crate) struct Open {
    span: Span,
    sampled: bool,
    previous: Option<Arc<TraceContext>>,
}

impl Open {
    pub(crate) fn set(&mut self, key: &str, value: Value) {
        if !self.sampled {
            return;
        }
        if let Some(slot) = self.span.attributes.iter_mut().find(|(k, _)| k == key) {
            slot.1 = value;
        } else {
            self.span.attributes.push((key.to_string(), value));
        }
    }

    pub(crate) fn fail(&mut self, message: &str) {
        self.span.error = Some(message.to_string());
    }

    pub(crate) fn rename(&mut self, name: String) {
        self.span.name = name;
    }
}

/// Start a span as a child of `parent` (else of the task's current span, else a new trace), and
/// make it the task's current span. `None` when tracing is off.
pub(crate) fn start(name: &str, kind: Kind, parent: Option<TraceContext>) -> Option<Box<Open>> {
    if !on() {
        return None;
    }
    let config = CONFIG.get()?;
    let previous = crate::log::current_trace();
    let parent = parent.or_else(|| previous.as_deref().cloned());
    let (trace_id, parent_id, sampled, state) = match parent {
        Some(p) => (p.trace_id, p.span_id, p.sampled, p.state),
        None => {
            let trace_id = (u128::from(nonzero_random()) << 64) | u128::from(random());
            let sampled = config.threshold == u64::MAX || (trace_id as u64) < config.threshold;
            (trace_id, 0, sampled, None)
        }
    };
    let span_id = nonzero_random();
    crate::log::set_trace(Some(Arc::new(TraceContext { trace_id, span_id, sampled, state })));
    Some(Box::new(Open {
        span: Span {
            trace_id,
            span_id,
            parent_id,
            name: name.to_string(),
            kind,
            start: unix_nanos(),
            end: 0,
            attributes: Vec::new(),
            error: None,
        },
        sampled,
        previous,
    }))
}

/// End a span: put back its parent as the task's current span, and queue it if it is sampled.
// Spans move as boxes, one pointer, between the runtime and the handles compiled code holds.
#[allow(clippy::boxed_local)]
pub(crate) fn end(open: Box<Open>) {
    let Open { mut span, sampled, previous } = *open;
    crate::log::set_trace(previous);
    if !sampled {
        return;
    }
    span.end = unix_nanos().max(span.start);
    let mut queue = QUEUE.lock().unwrap_or_else(|p| p.into_inner());
    if queue.len() >= MAX_QUEUE {
        DROPPED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    queue.push(span);
    if queue.len() >= BATCH {
        WAKE.notify_one();
    }
}

/// Drop a span without recording it, putting back its parent.
// Spans move as boxes, one pointer, between the runtime and the handles compiled code holds.
#[allow(clippy::boxed_local)]
pub(crate) fn discard(open: Box<Open>) {
    crate::log::set_trace(open.previous);
}

// ---- the server's spans -----------------------------------------------------------------

/// Start the span of a request a server is answering, continuing the trace in its
/// `traceparent` header if it has a usable one.
pub(crate) fn server_start(
    method: &str,
    path: &str,
    traceparent: Option<&str>,
    tracestate: Option<&str>,
) -> Option<Box<Open>> {
    let parent = traceparent.and_then(|h| parse_traceparent(h, tracestate));
    let mut open = start(method, Kind::Server, parent)?;
    open.set("http.request.method", Value::Text(method.to_string()));
    open.set("url.path", Value::Text(path.to_string()));
    Some(open)
}

/// End a server span, named `METHOD route` once the route is known.
pub(crate) fn server_end(mut open: Box<Open>, method: &str, route: Option<&str>, status: i64, client: Option<String>) {
    if let Some(route) = route {
        open.rename(format!("{method} {route}"));
        open.set("http.route", Value::Text(route.to_string()));
    }
    open.set("http.response.status_code", Value::Int(status));
    if let Some(client) = client {
        open.set("client.address", Value::Text(client));
    }
    if status >= 500 {
        open.fail(&format!("{status}"));
    }
    end(open);
}

// ---- the program's spans ------------------------------------------------------------------

/// Start a span of the program's own, `with trace.span(name):`. Returns its handle, or 0 when
/// tracing is off.
///
/// # Safety
/// `ptr` and `len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_trace_start(ptr: *const u8, len: i64) -> i64 {
    match start(unsafe { Str::from_raw(ptr, len) }, Kind::Internal, None) {
        Some(open) => Box::into_raw(open) as i64,
        None => 0,
    }
}

/// Set an attribute of a span started by [`lrt_trace_start`].
///
/// # Safety
/// `handle` is 0 or a span that has not ended; the pointers describe UTF-8 strings.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_trace_set(
    handle: i64,
    key: *const u8,
    key_len: i64,
    value: *const u8,
    value_len: i64,
) {
    if handle == 0 {
        return;
    }
    let open = unsafe { &mut *(handle as *mut Open) };
    let (key, value) = unsafe { (Str::from_raw(key, key_len), Str::from_raw(value, value_len)) };
    open.set(key, Value::Text(value.to_string()));
}

/// Mark a span as failed, with a message.
///
/// # Safety
/// `handle` is 0 or a span that has not ended; the pointer describes a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_trace_fail(handle: i64, ptr: *const u8, len: i64) {
    if handle == 0 {
        return;
    }
    let open = unsafe { &mut *(handle as *mut Open) };
    open.fail(unsafe { Str::from_raw(ptr, len) });
}

/// End a span started by [`lrt_trace_start`].
///
/// # Safety
/// `handle` is 0 or a span that has not ended, and is not used again.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_trace_end(handle: i64) {
    if handle != 0 {
        end(unsafe { Box::from_raw(handle as *mut Open) });
    }
}

/// The running task's trace ID, or `""`.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_trace_id() -> Str {
    let id = crate::log::current_trace().map(|c| hex(c.trace_id, 32)).unwrap_or_default();
    Str::from_bytes(id.as_bytes())
}

/// The running task's context as a `traceparent` header, or `""`.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_trace_traceparent() -> Str {
    let header = crate::log::current_trace().map(|c| traceparent(&c)).unwrap_or_default();
    Str::from_bytes(header.as_bytes())
}

// ---- turning it on --------------------------------------------------------------------------

/// The URL spans go to: `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` as it is, else the base endpoint
/// (the argument, else `OTEL_EXPORTER_OTLP_ENDPOINT`) with `/v1/traces`.
fn traces_url(endpoint: &str) -> Option<String> {
    let given = endpoint.trim();
    if given.is_empty()
        && let Ok(url) = std::env::var("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")
        && !url.trim().is_empty()
    {
        return Some(url.trim().to_string());
    }
    let base = if given.is_empty() { std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok()? } else { given.to_string() };
    let base = base.trim().trim_end_matches('/');
    (!base.is_empty()).then(|| format!("{base}/v1/traces"))
}

/// The service's name: `OTEL_SERVICE_NAME`, else the argument, else the program's file name.
fn service_name(service: &str) -> String {
    if let Ok(name) = std::env::var("OTEL_SERVICE_NAME")
        && !name.trim().is_empty()
    {
        return name.trim().to_string();
    }
    if !service.trim().is_empty() {
        return service.trim().to_string();
    }
    std::env::args()
        .next()
        .and_then(|p| std::path::Path::new(&p).file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "unknown_service".to_string())
}

/// The sampling ratio: the argument if it is from 0 to 1, else `OTEL_TRACES_SAMPLER_ARG`, else 1.
fn ratio(given: f64) -> Result<f64, String> {
    if (0.0..=1.0).contains(&given) {
        return Ok(given);
    }
    if !given.is_nan() && given >= 0.0 {
        return Err(format!("the sampling ratio must be from 0 to 1, not {given}"));
    }
    match std::env::var("OTEL_TRACES_SAMPLER_ARG") {
        Ok(text) if !text.trim().is_empty() => match text.trim().parse::<f64>() {
            Ok(r) if (0.0..=1.0).contains(&r) => Ok(r),
            _ => Err(format!("OTEL_TRACES_SAMPLER_ARG must be a number from 0 to 1, not `{}`", text.trim())),
        },
        _ => Ok(1.0),
    }
}

/// Turn tracing on. `service` and `endpoint` may be `""`, and `given_ratio` negative, to take
/// them from the environment. Returns 1 if tracing is on, 0 if there is no endpoint (so it
/// stays off), or -1 after recording an error.
///
/// # Safety
/// The pointers describe UTF-8 strings.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_trace_init(
    service: *const u8,
    service_len: i64,
    endpoint: *const u8,
    endpoint_len: i64,
    given_ratio: f64,
) -> i64 {
    let (service, endpoint) = unsafe { (Str::from_raw(service, service_len), Str::from_raw(endpoint, endpoint_len)) };
    if let Some(config) = CONFIG.get() {
        // Once per process: a second call (`request_log(trace=True)` after `trace.init`) keeps
        // the first's settings.
        let _ = config;
        return 1;
    }
    let ratio = match ratio(given_ratio) {
        Ok(r) => r,
        Err(message) => {
            crate::sys::record_error(message);
            return -1;
        }
    };
    let Some(url) = traces_url(endpoint) else { return 0 };
    if !url.starts_with("http://") && !url.starts_with("https://") {
        crate::sys::record_error(format!("the trace endpoint must be an http:// or https:// URL, not `{url}`"));
        return -1;
    }
    let threshold = if ratio >= 1.0 { u64::MAX } else { (ratio * u64::MAX as f64) as u64 };
    let interval = std::env::var(lang_names::env_var("TRACE_INTERVAL_MS"))
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map_or(INTERVAL, |ms| Duration::from_millis(ms.max(10)));
    let config = Config { service: service_name(service), url, threshold, exporting: Mutex::new(()), interval };
    if CONFIG.set(config).is_err() {
        return 1;
    }
    let _ = std::thread::Builder::new().name("trace-export".to_string()).spawn(export_loop);
    let _ = AT_EXIT.set(shutdown);
    ON.store(true, Ordering::Relaxed);
    1
}

/// Send the waiting spans now, and wait for the collector (up to 5 s).
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_trace_flush() {
    shutdown();
}

/// What to do when the program ends, set by [`lrt_trace_init`]. The exporter is reached only
/// through it, so a program that never turns tracing on does not link the exporter, nor the
/// HTTP client and TLS it sends with.
static AT_EXIT: OnceLock<fn()> = OnceLock::new();

/// Send what is left, if tracing is on, before the program ends.
pub(crate) fn at_exit() {
    if let Some(flush) = AT_EXIT.get() {
        flush();
    }
}

/// Send what is left before the program ends.
fn shutdown() {
    if !on() {
        return;
    }
    let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
    while Instant::now() < deadline {
        if !export_batch(deadline.saturating_duration_since(Instant::now())) {
            break;
        }
    }
}

// ---- export ---------------------------------------------------------------------------------

fn export_loop() {
    let Some(config) = CONFIG.get() else { return };
    loop {
        {
            let queue = QUEUE.lock().unwrap_or_else(|p| p.into_inner());
            if queue.len() < BATCH {
                let _ = WAKE.wait_timeout(queue, config.interval);
            }
        }
        while export_batch(EXPORT_TIMEOUT) {
            let queue = QUEUE.lock().unwrap_or_else(|p| p.into_inner());
            if queue.len() < BATCH {
                break;
            }
        }
    }
}

/// Send one batch; false if there was nothing to send.
fn export_batch(timeout: Duration) -> bool {
    let Some(config) = CONFIG.get() else { return false };
    let _exporting = config.exporting.lock().unwrap_or_else(|p| p.into_inner());
    let batch: Vec<Span> = {
        let mut queue = QUEUE.lock().unwrap_or_else(|p| p.into_inner());
        let n = queue.len().min(BATCH);
        queue.drain(..n).collect()
    };
    if batch.is_empty() {
        return false;
    }
    let body = encode(&config.service, &batch);
    let headers = "Content-Type: application/json\r\n";
    let result = crate::client::send("POST", &config.url, headers, body.as_bytes(), timeout);
    match result {
        Ok(status) if (200..300).contains(&status) => {
            EXPORTED.fetch_add(batch.len() as u64, Ordering::Relaxed);
            if FAILING.swap(false, Ordering::Relaxed) {
                crate::log::write_line("info", "exporting traces again", "");
            }
        }
        failure => {
            DROPPED.fetch_add(batch.len() as u64, Ordering::Relaxed);
            if !FAILING.swap(true, Ordering::Relaxed) {
                let why = match failure {
                    Ok(status) => format!("the collector answered {status}"),
                    Err(message) => message,
                };
                crate::log::write_line(
                    "warn",
                    &format!(
                        "could not send {} spans to {}: {why}; dropping them until it works",
                        batch.len(),
                        config.url
                    ),
                    "",
                );
            }
        }
    }
    true
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
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn push_attribute(out: &mut String, key: &str, value: &Value) {
    out.push_str("{\"key\":");
    push_json_string(out, key);
    out.push_str(",\"value\":{");
    match value {
        Value::Text(text) => {
            out.push_str("\"stringValue\":");
            push_json_string(out, text);
        }
        // OTLP/JSON writes 64-bit integers as strings.
        Value::Int(n) => out.push_str(&format!("\"intValue\":\"{n}\"")),
    }
    out.push_str("}}");
}

/// The OTLP/JSON body for a batch of spans.
pub(crate) fn encode(service: &str, spans: &[Span]) -> String {
    let mut out = String::with_capacity(256 + spans.len() * 400);
    out.push_str("{\"resourceSpans\":[{\"resource\":{\"attributes\":[");
    push_attribute(&mut out, "service.name", &Value::Text(service.to_string()));
    out.push(',');
    push_attribute(&mut out, "telemetry.sdk.name", &Value::Text(lang_names::CLI_NAME.to_string()));
    out.push(',');
    push_attribute(&mut out, "telemetry.sdk.version", &Value::Text(env!("CARGO_PKG_VERSION").to_string()));
    out.push_str("]},\"scopeSpans\":[{\"scope\":{\"name\":");
    push_json_string(&mut out, lang_names::CLI_NAME);
    out.push_str(",\"version\":");
    push_json_string(&mut out, env!("CARGO_PKG_VERSION"));
    out.push_str("},\"spans\":[");
    for (i, span) in spans.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"traceId\":\"{}\",\"spanId\":\"{}\"",
            hex(span.trace_id, 32),
            hex(u128::from(span.span_id), 16)
        ));
        if span.parent_id != 0 {
            out.push_str(&format!(",\"parentSpanId\":\"{}\"", hex(u128::from(span.parent_id), 16)));
        }
        out.push_str(",\"name\":");
        push_json_string(&mut out, &span.name);
        out.push_str(&format!(
            ",\"kind\":{},\"startTimeUnixNano\":\"{}\",\"endTimeUnixNano\":\"{}\",\"attributes\":[",
            span.kind as u8, span.start, span.end
        ));
        for (j, (key, value)) in span.attributes.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            push_attribute(&mut out, key, value);
        }
        out.push(']');
        if let Some(message) = &span.error {
            out.push_str(",\"status\":{\"code\":2,\"message\":");
            push_json_string(&mut out, message);
            out.push('}');
        }
        out.push('}');
    }
    out.push_str("]}]}]}");
    out
}

// ---- PostgreSQL ---------------------------------------------------------------------------

/// The statement's first word, upper-cased (`SELECT`, `INSERT`, ...), for a query span's name:
/// never the parameters or the rest of the text, which may hold values.
pub(crate) fn operation(sql: &str) -> String {
    let sql = sql.trim_start();
    let word: String = sql.chars().take_while(|c| c.is_ascii_alphabetic()).take(16).collect();
    if word.is_empty() { "QUERY".to_string() } else { word.to_ascii_uppercase() }
}

/// Start the span of a PostgreSQL query or script.
pub(crate) fn query_start(sql: &str) -> Option<Box<Open>> {
    if !on() {
        return None;
    }
    let operation = operation(sql);
    let mut open = start(&operation, Kind::Client, None)?;
    open.set("db.system", Value::Text("postgresql".to_string()));
    open.set("db.operation.name", Value::Text(operation));
    Some(open)
}

/// End a query's span, failed if the query was.
pub(crate) fn query_end(open: Option<Box<Open>>, ok: bool) {
    if let Some(mut open) = open {
        if !ok {
            open.fail("the query failed");
        }
        end(open);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traceparent_parsing() {
        let header = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
        let context = parse_traceparent(header, Some("congo=t61rcWkgMzE")).unwrap();
        assert_eq!(context.trace_id, 0x4bf92f3577b34da6a3ce929d0e0e4736);
        assert_eq!(context.span_id, 0x00f067aa0ba902b7);
        assert!(context.sampled);
        assert_eq!(context.state.as_deref(), Some("congo=t61rcWkgMzE"));
        assert_eq!(traceparent(&context), header);
        let unsampled = parse_traceparent("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-00", None).unwrap();
        assert!(!unsampled.sampled);
        // A later version may add fields.
        assert!(parse_traceparent("01-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01-extra", None).is_some());
        for bad in [
            "",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01-extra",
            "00-00000000000000000000000000000000-00f067aa0ba902b7-01",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01",
            "00-4BF92F3577B34DA6A3CE929D0E0E4736-00f067aa0ba902b7-01",
            "ff-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            "00-4bf92f3577b34da6a3ce929d0e0e473-00f067aa0ba902b7-01",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-1",
        ] {
            assert!(parse_traceparent(bad, None).is_none(), "{bad}");
        }
    }

    #[test]
    fn operations_are_the_first_word() {
        assert_eq!(operation("  select * from t where id = $1"), "SELECT");
        assert_eq!(operation("INSERT INTO t VALUES ('secret')"), "INSERT");
        assert_eq!(operation("with x as (select 1) select * from x"), "WITH");
        assert_eq!(operation("-- comment\nselect 1"), "QUERY");
        assert_eq!(operation(""), "QUERY");
    }

    #[test]
    fn otlp_json() {
        let span = Span {
            trace_id: 0x0af7651916cd43dd8448eb211c80319c,
            span_id: 0xb7ad6b7169203331,
            parent_id: 0x00f067aa0ba902b7,
            name: "GET /items/{id}".to_string(),
            kind: Kind::Server,
            start: 1_700_000_000_000_000_000,
            end: 1_700_000_000_001_000_000,
            attributes: vec![
                ("http.request.method".to_string(), Value::Text("GET".to_string())),
                ("http.response.status_code".to_string(), Value::Int(500)),
            ],
            error: Some("500 \"boom\"".to_string()),
        };
        let json = encode("photos", &[span]);
        assert!(json.starts_with("{\"resourceSpans\":[{\"resource\":{\"attributes\":[{\"key\":\"service.name\",\"value\":{\"stringValue\":\"photos\"}}"), "{json}");
        assert!(json.contains("\"traceId\":\"0af7651916cd43dd8448eb211c80319c\",\"spanId\":\"b7ad6b7169203331\",\"parentSpanId\":\"00f067aa0ba902b7\",\"name\":\"GET /items/{id}\",\"kind\":2,\"startTimeUnixNano\":\"1700000000000000000\",\"endTimeUnixNano\":\"1700000000001000000\""), "{json}");
        assert!(json.contains("{\"key\":\"http.response.status_code\",\"value\":{\"intValue\":\"500\"}}"), "{json}");
        assert!(json.contains("\"status\":{\"code\":2,\"message\":\"500 \\\"boom\\\"\"}"), "{json}");
        assert!(json.ends_with("}]}]}]}"), "{json}");
    }

    #[test]
    fn the_url_and_ratio() {
        assert_eq!(traces_url("http://collector:4318/").as_deref(), Some("http://collector:4318/v1/traces"));
        assert_eq!(ratio(0.25), Ok(0.25));
        assert!(ratio(1.5).is_err());
    }
}
