//! Metrics (docs/14-production-web-plan.md, 4.7): the HTTP server's requests, the program's own
//! counters, gauges and histograms, and figures about the process, served in the Prometheus text
//! format (version 0.0.4).
//!
//! **HTTP requests** are recorded only once `server.metrics` has turned recording on: until then
//! the request path reads no clock and touches no counter. Recording takes no lock and allocates
//! nothing. Each worker thread writes to its own shard (one of [`SHARDS`]), so threads do not
//! fight over cache lines; a scrape adds the shards together.
//!
//! - Per route, a histogram of durations, the response bytes, and the count, in a block of
//!   atomics padded to cache lines. Routes are labelled by method and pattern, never by path:
//!   `lrt_http_route_slot` gives each distinct `METHOD pattern` a slot when the route is added,
//!   up to [`MAX_ROUTES`]; later ones share the slot labelled `other`. Requests no route took
//!   are labelled `unmatched`.
//! - Per route, method and status, a count, in a small open-addressing table per shard whose
//!   keys are claimed with a compare-and-swap. Statuses are the exact codes seen.
//!
//! **The program's metrics** live in a registry of [`Metric`]s, each with up to
//! [`MAX_LABEL_VALUES`] series. An update takes a read lock and changes atomics; only a new
//! combination of label values takes the write lock.

use crate::Str;
use std::cell::Cell;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::Duration;

static ON: AtomicBool = AtomicBool::new(false);

/// Whether HTTP requests are being recorded.
#[inline]
pub(crate) fn on() -> bool {
    ON.load(Ordering::Relaxed)
}

// ---- route labels ---------------------------------------------------------------------------

/// Routes with labels of their own; the rest share [`OTHER_ROUTE`].
pub(crate) const MAX_ROUTES: usize = 1024;
pub(crate) const OTHER_ROUTE: u16 = MAX_ROUTES as u16;
/// Requests that no route took.
pub(crate) const UNMATCHED: u16 = OTHER_ROUTE + 1;
const ROUTE_SLOTS: usize = UNMATCHED as usize + 1;

/// Each slot's method and pattern, set once and never freed, so that a request can hand its
/// pattern to compiled code without copying it.
static LABELS: [OnceLock<(&'static str, &'static str)>; MAX_ROUTES] = [const { OnceLock::new() }; MAX_ROUTES];
static INTERNED: Mutex<Option<HashMap<String, u16>>> = Mutex::new(None);

/// The slot for a route, the same for every route with this method and pattern.
fn route_slot(method: &str, pattern: &str) -> u16 {
    let mut interned = INTERNED.lock().unwrap_or_else(|p| p.into_inner());
    let interned = interned.get_or_insert_with(HashMap::new);
    let key = format!("{method} {pattern}");
    if let Some(&slot) = interned.get(&key) {
        return slot;
    }
    let slot = interned.len();
    if slot >= MAX_ROUTES {
        if slot == MAX_ROUTES {
            crate::log::write_line(
                "warn",
                &format!("more than {MAX_ROUTES} routes: the metrics of the rest are labelled route=\"other\""),
                "",
            );
            interned.insert(String::new(), OTHER_ROUTE);
        }
        return OTHER_ROUTE;
    }
    let method: &'static str = Box::leak(method.to_string().into_boxed_str());
    let pattern: &'static str = Box::leak(pattern.to_string().into_boxed_str());
    let _ = LABELS[slot].set((method, pattern));
    interned.insert(key, slot as u16);
    slot as u16
}

/// The pattern of a route's slot, or `None` for [`UNMATCHED`].
pub(crate) fn route_pattern(slot: u16) -> Option<&'static str> {
    match slot {
        OTHER_ROUTE => Some("other"),
        _ => LABELS.get(slot as usize).and_then(OnceLock::get).map(|(_, pattern)| *pattern),
    }
}

/// The slot for a route with this method and pattern. Called when a route is added.
///
/// # Safety
/// The pointer and length pairs describe UTF-8 strings.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_http_route_slot(
    method: *const u8,
    method_len: i64,
    pattern: *const u8,
    pattern_len: i64,
) -> i64 {
    let (method, pattern) = unsafe { (Str::from_raw(method, method_len), Str::from_raw(pattern, pattern_len)) };
    i64::from(route_slot(method, pattern))
}

// ---- HTTP recording -------------------------------------------------------------------------

/// Upper bounds of the duration buckets, in nanoseconds: 0.1 ms to 10 s, finer than
/// Prometheus' defaults because handlers here take microseconds.
const BUCKET_NS: [u64; 16] = [
    100_000,
    250_000,
    500_000,
    1_000_000,
    2_500_000,
    5_000_000,
    10_000_000,
    25_000_000,
    50_000_000,
    100_000_000,
    250_000_000,
    500_000_000,
    1_000_000_000,
    2_500_000_000,
    5_000_000_000,
    10_000_000_000,
];
/// The same bounds in seconds, as the text format writes them, and the default buckets of a
/// program's histograms.
pub(crate) const BUCKET_SECONDS: [f64; 16] =
    [0.0001, 0.00025, 0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0];

/// Shards of the request counters. A thread takes one when it first records.
const SHARDS: usize = 16;
/// Series of (route, method, status) per shard.
const SERIES: usize = 4096;

/// One route's timings in one shard: a count per bucket (the last for longer than 10 s), the
/// total time and the response bytes. Padded so that two routes never share a cache line.
#[repr(align(64))]
struct RouteCells {
    buckets: [AtomicU64; 17],
    nanos: AtomicU64,
    bytes: AtomicU64,
}

/// A count of requests with one route, method and status: `key` is 0 until claimed.
struct Series {
    key: AtomicU64,
    count: AtomicU64,
}

struct Shard {
    routes: Box<[RouteCells]>,
    series: Box<[Series]>,
}

static TABLES: OnceLock<Box<[Shard]>> = OnceLock::new();
/// Requests whose series found no room (never, unless something mints statuses).
static UNRECORDED: AtomicU64 = AtomicU64::new(0);
static IN_FLIGHT: AtomicI64 = AtomicI64::new(0);
static CONNECTIONS: AtomicU64 = AtomicU64::new(0);
/// Requests that failed before or while being handled, by [`ErrorKind`].
static ERRORS: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];
static NEXT_SHARD: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static SHARD: Cell<usize> = const { Cell::new(usize::MAX) };
}

/// A slice of `len` values whose all-zero bytes are valid (atomics). The memory comes zeroed from
/// the system, so the pages of routes that never see a request are never touched.
fn zeroed<T>(len: usize) -> Box<[T]> {
    let layout = std::alloc::Layout::array::<T>(len).expect("a small table");
    // SAFETY: a non-zero size; `T` is made of atomics, for which all-zero bytes are valid.
    unsafe {
        let ptr = std::alloc::alloc_zeroed(layout) as *mut T;
        if ptr.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len))
    }
}

fn tables() -> &'static [Shard] {
    TABLES.get_or_init(|| (0..SHARDS).map(|_| Shard { routes: zeroed(ROUTE_SLOTS), series: zeroed(SERIES) }).collect())
}

fn shard() -> &'static Shard {
    let index = SHARD.with(|shard| {
        let mut index = shard.get();
        if index == usize::MAX {
            index = NEXT_SHARD.fetch_add(1, Ordering::Relaxed) % SHARDS;
            shard.set(index);
        }
        index
    });
    &tables()[index]
}

/// Turn on the recording of HTTP requests.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_metrics_enable() {
    tables();
    ON.store(true, Ordering::SeqCst);
}

pub(crate) fn connection_opened() {
    CONNECTIONS.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn request_started() {
    IN_FLIGHT.fetch_add(1, Ordering::Relaxed);
}

/// A request to a route left out of the metrics has been answered.
pub(crate) fn request_done_unmetered() {
    IN_FLIGHT.fetch_sub(1, Ordering::Relaxed);
}

/// Why a request failed.
#[derive(Clone, Copy)]
pub(crate) enum ErrorKind {
    Panic = 0,
    Timeout = 1,
    Parse = 2,
    TooLarge = 3,
    /// The client hung up while the handler ran (`cancel.rs`).
    ClientClosed = 4,
}

const ERROR_KINDS: [&str; 5] = ["panic", "timeout", "parse", "too_large", "client_closed"];

pub(crate) fn request_failed(kind: ErrorKind) {
    ERRORS[kind as usize].fetch_add(1, Ordering::Relaxed);
}

/// The methods with labels of their own, in the order of `http::METHODS`; others are `_OTHER`.
const METHOD_LABELS: [&str; 8] = ["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS", "_OTHER"];

pub(crate) fn method_index(method: &str) -> u8 {
    METHOD_LABELS[..7].iter().position(|m| *m == method).unwrap_or(7) as u8
}

fn series_key(route: u16, method: u8, status: u16) -> u64 {
    1 + ((u64::from(route) << 24) | (u64::from(method) << 16) | u64::from(status))
}

fn key_parts(key: u64) -> (u16, u8, u16) {
    let key = key - 1;
    ((key >> 24) as u16, ((key >> 16) & 0xff) as u8, (key & 0xffff) as u16)
}

/// Record an answered request. `elapsed` is `None` for a WebSocket upgrade, whose handler ran
/// for the life of the connection: it is counted but not timed.
pub(crate) fn request_finished(route: u16, method: u8, status: i64, elapsed: Option<Duration>, bytes: u64) {
    IN_FLIGHT.fetch_sub(1, Ordering::Relaxed);
    let shard = shard();
    let route = route.min(UNMATCHED);
    let key = series_key(route, method, status.clamp(0, 999) as u16);
    let mask = SERIES - 1;
    let mut at = (key.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 52) as usize & mask;
    let mut counted = false;
    for _ in 0..SERIES {
        let series = &shard.series[at];
        let found = series.key.load(Ordering::Acquire);
        if found == key
            || (found == 0
                && match series.key.compare_exchange(0, key, Ordering::AcqRel, Ordering::Acquire) {
                    Ok(_) => true,
                    Err(other) => other == key,
                })
        {
            series.count.fetch_add(1, Ordering::Relaxed);
            counted = true;
            break;
        }
        at = (at + 1) & mask;
    }
    if !counted {
        UNRECORDED.fetch_add(1, Ordering::Relaxed);
    }
    let Some(elapsed) = elapsed else { return };
    let cells = &shard.routes[route as usize];
    let nanos = elapsed.as_nanos().min(u128::from(u64::MAX)) as u64;
    let bucket = BUCKET_NS.iter().position(|&bound| nanos <= bound).unwrap_or(BUCKET_NS.len());
    cells.buckets[bucket].fetch_add(1, Ordering::Relaxed);
    cells.nanos.fetch_add(nanos, Ordering::Relaxed);
    cells.bytes.fetch_add(bytes, Ordering::Relaxed);
}

// ---- figures for the dashboard ---------------------------------------------------------------

/// One route slot's requests since recording began: the count, by status class (1xx to 5xx),
/// and the durations by bucket.
#[derive(Clone, Copy, Default)]
pub(crate) struct RouteTotal {
    pub(crate) slot: u16,
    pub(crate) requests: u64,
    pub(crate) by_class: [u64; 5],
    pub(crate) buckets: [u64; 17],
}

/// The totals of every slot that has seen a request, in slot order.
pub(crate) fn route_totals() -> Vec<RouteTotal> {
    let Some(shards) = TABLES.get() else { return Vec::new() };
    let mut totals: Vec<RouteTotal> =
        (0..ROUTE_SLOTS as u16).map(|slot| RouteTotal { slot, ..Default::default() }).collect();
    for shard in shards.iter() {
        for series in shard.series.iter() {
            let key = series.key.load(Ordering::Acquire);
            if key == 0 {
                continue;
            }
            let (route, _, status) = key_parts(key);
            let count = series.count.load(Ordering::Relaxed);
            let total = &mut totals[usize::from(route.min(UNMATCHED))];
            total.requests += count;
            total.by_class[usize::from(status / 100).clamp(1, 5) - 1] += count;
        }
        for (total, cells) in totals.iter_mut().zip(shard.routes.iter()) {
            for (sum, cell) in total.buckets.iter_mut().zip(&cells.buckets) {
                *sum += cell.load(Ordering::Relaxed);
            }
        }
    }
    totals.retain(|t| t.requests > 0 || t.buckets.iter().any(|&b| b > 0));
    totals
}

/// A slot's method (empty for `other` and `unmatched`) and pattern.
pub(crate) fn route_name(slot: u16) -> (&'static str, &'static str) {
    match slot {
        OTHER_ROUTE => ("", "other"),
        UNMATCHED => ("", "unmatched"),
        _ => LABELS.get(usize::from(slot)).and_then(OnceLock::get).copied().unwrap_or(("", "unmatched")),
    }
}

pub(crate) fn in_flight() -> i64 {
    IN_FLIGHT.load(Ordering::Relaxed).max(0)
}

/// Each of the program's metrics: its name, kind and help, and its value added across its
/// series (a histogram's count of observations).
pub(crate) fn program_values() -> Vec<(String, &'static str, String, f64)> {
    let registry = REGISTRY.read().unwrap_or_else(|p| p.into_inner());
    registry
        .iter()
        .map(|metric| {
            let all = metric.series.read().unwrap_or_else(|p| p.into_inner());
            let value = all
                .values()
                .map(|series| match metric.kind {
                    Kind::Histogram => series.count.load(Ordering::Relaxed) as f64,
                    _ => f64::from_bits(series.value.load(Ordering::Relaxed)),
                })
                .sum();
            (metric.name.clone(), metric.kind.name(), metric.help.clone(), value)
        })
        .collect()
}

// ---- the program's metrics ------------------------------------------------------------------

/// Label values beyond this many per metric are folded into one series labelled `other`.
pub(crate) const MAX_LABEL_VALUES: usize = 1000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Counter,
    Gauge,
    Histogram,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Counter => "counter",
            Kind::Gauge => "gauge",
            Kind::Histogram => "histogram",
        }
    }
}

/// One series: a counter's or gauge's value (an `f64`'s bits), or a histogram's sum, count and
/// buckets (one per bound, the last for values above every bound).
struct Values {
    value: AtomicU64,
    count: AtomicU64,
    buckets: Box<[AtomicU64]>,
}

struct Metric {
    name: String,
    help: String,
    kind: Kind,
    labels: Vec<String>,
    bounds: Vec<f64>,
    /// By the label values, each followed by a NUL.
    series: RwLock<HashMap<Box<str>, Values>>,
    warned: AtomicBool,
}

static REGISTRY: RwLock<Vec<Metric>> = RwLock::new(Vec::new());

/// Families the runtime writes itself.
const RESERVED: [&str; 16] = [
    "http_requests_total",
    "http_request_duration_seconds",
    "http_response_size_bytes",
    "http_requests_in_flight",
    "http_connections_open",
    "http_connections_total",
    "http_request_errors_total",
    "process_start_time_seconds",
    "process_resident_memory_bytes",
    "process_cpu_seconds_total",
    "runtime_heap_bytes",
    "runtime_gc_collections_total",
    "runtime_tasks",
    "runtime_worker_threads",
    "runtime_unrecorded_requests_total",
    "build_info",
];

fn valid_metric_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == ':')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
}

fn valid_label_name(name: &str) -> bool {
    let mut chars = name.chars();
    !name.starts_with("__")
        && chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Register a metric, or find the one already registered with this name, kind, labels and
/// buckets. Returns its id, or an error message.
fn register(kind: Kind, name: &str, help: &str, labels: Vec<String>, bounds: Vec<f64>) -> Result<usize, String> {
    if !valid_metric_name(name) {
        return Err(format!(
            "{name:?} is not a metric name: use letters, digits, `_` and `:`, not starting with a digit"
        ));
    }
    if RESERVED.contains(&name) {
        return Err(format!("the metric {name} is the runtime's own"));
    }
    for (i, label) in labels.iter().enumerate() {
        if !valid_label_name(label) {
            return Err(format!(
                "{label:?} is not a label name: use letters, digits and `_`, not starting with a digit or `__`"
            ));
        }
        if labels[..i].contains(label) {
            return Err(format!("the label {label:?} is named twice"));
        }
        if kind == Kind::Histogram && label == "le" {
            return Err("a histogram cannot have a label called \"le\", which holds its bucket bounds".to_string());
        }
    }
    let bounds = if kind == Kind::Histogram && bounds.is_empty() { BUCKET_SECONDS.to_vec() } else { bounds };
    if bounds.iter().any(|b| !b.is_finite()) || bounds.windows(2).any(|w| w[0] >= w[1]) {
        return Err("histogram buckets must be finite and increasing".to_string());
    }
    let mut registry = REGISTRY.write().unwrap_or_else(|p| p.into_inner());
    if let Some(id) = registry.iter().position(|m| m.name == name) {
        let existing = &registry[id];
        if existing.kind == kind && existing.labels == labels && existing.bounds == bounds {
            return Ok(id);
        }
        return Err(format!(
            "the metric {name} is already registered as a {} with the labels {:?}",
            existing.kind.name(),
            existing.labels
        ));
    }
    registry.push(Metric {
        name: name.to_string(),
        help: help.to_string(),
        kind,
        labels,
        bounds,
        series: RwLock::new(HashMap::new()),
        warned: AtomicBool::new(false),
    });
    Ok(registry.len() - 1)
}

fn add_f64(cell: &AtomicU64, amount: f64) {
    let mut current = cell.load(Ordering::Relaxed);
    loop {
        let next = (f64::from_bits(current) + amount).to_bits();
        match cell.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return,
            Err(now) => current = now,
        }
    }
}

#[derive(Clone, Copy)]
enum Update {
    Add(f64),
    Set(f64),
    Observe(f64),
}

/// Change one series of metric `id`. `values` are its label values, each followed by a NUL.
fn update(id: usize, values: &str, change: Update) -> Result<(), String> {
    let registry = REGISTRY.read().unwrap_or_else(|p| p.into_inner());
    let metric = registry.get(id).ok_or("no such metric")?;
    match (metric.kind, change) {
        (Kind::Counter, Update::Add(amount)) if amount < 0.0 || amount.is_nan() => {
            return Err(format!("the counter {} only goes up, so it cannot be increased by {amount}", metric.name));
        }
        (Kind::Counter, Update::Add(_))
        | (Kind::Gauge, Update::Add(_) | Update::Set(_))
        | (Kind::Histogram, Update::Observe(_)) => {}
        _ => return Err(format!("the {} {} cannot be changed that way", metric.kind.name(), metric.name)),
    }
    if values.matches('\0').count() != metric.labels.len() {
        return Err(format!("the metric {} takes the labels {:?}", metric.name, metric.labels));
    }
    let apply = |series: &Values| match change {
        Update::Add(amount) => add_f64(&series.value, amount),
        Update::Set(value) => series.value.store(value.to_bits(), Ordering::Relaxed),
        Update::Observe(value) => {
            let bucket = metric.bounds.iter().position(|&bound| value <= bound).unwrap_or(metric.bounds.len());
            series.buckets[bucket].fetch_add(1, Ordering::Relaxed);
            series.count.fetch_add(1, Ordering::Relaxed);
            add_f64(&series.value, value);
        }
    };
    if let Some(series) = metric.series.read().unwrap_or_else(|p| p.into_inner()).get(values) {
        apply(series);
        return Ok(());
    }
    let mut all = metric.series.write().unwrap_or_else(|p| p.into_inner());
    let key: Box<str> = if all.len() >= MAX_LABEL_VALUES && !all.contains_key(values) {
        if !metric.warned.swap(true, Ordering::Relaxed) {
            crate::log::write_line(
                "warn",
                &format!(
                    "the metric {} has more than {MAX_LABEL_VALUES} sets of label values; new ones are counted with the values \"other\"",
                    metric.name
                ),
                "",
            );
        }
        "other\0".repeat(metric.labels.len()).into_boxed_str()
    } else {
        values.into()
    };
    let series = all.entry(key).or_insert_with(|| Values {
        value: AtomicU64::new(0f64.to_bits()),
        count: AtomicU64::new(0),
        buckets: (0..=metric.bounds.len()).map(|_| AtomicU64::new(0)).collect(),
    });
    apply(series);
    Ok(())
}

/// Register a counter (0), gauge (1) or histogram (2). `labels` are the label names and
/// `buckets` the histogram's bounds (empty for the default ones), each followed by a NUL.
/// Returns the id, or -1 after recording the error.
///
/// # Safety
/// The pointer and length pairs describe UTF-8 strings.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C-unwind" fn lrt_metrics_register(
    kind: i64,
    name: *const u8,
    name_len: i64,
    help: *const u8,
    help_len: i64,
    labels: *const u8,
    labels_len: i64,
    buckets: *const u8,
    buckets_len: i64,
) -> i64 {
    let (name, help) = unsafe { (Str::from_raw(name, name_len), Str::from_raw(help, help_len)) };
    let (labels, buckets) = unsafe { (Str::from_raw(labels, labels_len), Str::from_raw(buckets, buckets_len)) };
    let kind = match kind {
        0 => Kind::Counter,
        1 => Kind::Gauge,
        _ => Kind::Histogram,
    };
    let labels: Vec<String> = labels.split_terminator('\0').map(str::to_string).collect();
    let bounds: Vec<f64> = buckets.split_terminator('\0').filter_map(|b| b.parse().ok()).collect();
    match register(kind, name, help, labels, bounds) {
        Ok(id) => id as i64,
        Err(message) => {
            crate::sys::record_error(message);
            -1
        }
    }
}

/// Add to a counter or gauge (0), set a gauge (1), or observe a value in a histogram (2).
/// Returns "" or what was wrong.
///
/// # Safety
/// `values` and `values_len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_metrics_update(
    id: i64,
    op: i64,
    values: *const u8,
    values_len: i64,
    amount: f64,
) -> Str {
    let values = unsafe { Str::from_raw(values, values_len) };
    let change = match op {
        0 => Update::Add(amount),
        1 => Update::Set(amount),
        _ => Update::Observe(amount),
    };
    match update(id.max(0) as usize, values, change) {
        Ok(()) => Str::empty(),
        Err(message) => Str::from_bytes(message.as_bytes()),
    }
}

// ---- the text format ------------------------------------------------------------------------

fn push_escaped(out: &mut String, text: &str, quote: bool) {
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '"' if quote => out.push_str("\\\""),
            c => out.push(c),
        }
    }
}

fn push_number(out: &mut String, value: f64) {
    if value.is_nan() {
        out.push_str("NaN");
    } else if value.is_infinite() {
        out.push_str(if value > 0.0 { "+Inf" } else { "-Inf" });
    } else {
        let _ = write!(out, "{value}");
    }
}

fn family(out: &mut String, name: &str, kind: &str, help: &str) {
    out.push_str("# HELP ");
    out.push_str(name);
    out.push(' ');
    push_escaped(out, help, false);
    out.push_str("\n# TYPE ");
    out.push_str(name);
    out.push(' ');
    out.push_str(kind);
    out.push('\n');
}

/// One sample: `name{label="value",...} number`. `labels` is already written as
/// `name="value",...`, or empty.
fn sample(out: &mut String, name: &str, labels: &str, extra: Option<(&str, &str)>, value: f64) {
    out.push_str(name);
    if !labels.is_empty() || extra.is_some() {
        out.push('{');
        out.push_str(labels);
        if let Some((label, text)) = extra {
            if !labels.is_empty() {
                out.push(',');
            }
            out.push_str(label);
            out.push_str("=\"");
            push_escaped(out, text, true);
            out.push('"');
        }
        out.push('}');
    }
    out.push(' ');
    push_number(out, value);
    out.push('\n');
}

fn labels_text(pairs: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (i, (name, value)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(name);
        out.push_str("=\"");
        push_escaped(&mut out, value, true);
        out.push('"');
    }
    out
}

/// A route slot's labels: its method and pattern, or only `route` for `other` and `unmatched`.
fn route_labels(slot: u16) -> Vec<(&'static str, &'static str)> {
    match slot {
        OTHER_ROUTE => vec![("route", "other")],
        UNMATCHED => vec![("route", "unmatched")],
        _ => match LABELS.get(slot as usize).and_then(OnceLock::get) {
            Some((method, pattern)) => vec![("method", method), ("route", pattern)],
            None => vec![("route", "unmatched")],
        },
    }
}

fn write_http(out: &mut String) {
    let Some(shards) = TABLES.get() else { return };
    let mut counts: HashMap<u64, u64> = HashMap::new();
    for shard in shards.iter() {
        for series in shard.series.iter() {
            let key = series.key.load(Ordering::Acquire);
            if key != 0 {
                *counts.entry(key).or_default() += series.count.load(Ordering::Relaxed);
            }
        }
    }
    let mut keys: Vec<u64> = counts.keys().copied().collect();
    keys.sort_unstable();
    family(out, "http_requests_total", "counter", "Requests answered, by method, route pattern and status.");
    for key in keys {
        let (route, method, status) = key_parts(key);
        let mut pairs = vec![("method", METHOD_LABELS[method as usize])];
        pairs.push(("route", route_labels(route).last().map_or("unmatched", |(_, pattern)| pattern)));
        let status = status.to_string();
        pairs.push(("status", &status));
        sample(out, "http_requests_total", &labels_text(&pairs), None, counts[&key] as f64);
    }
    // Per route: the buckets, time and bytes, added across shards.
    let mut routes: Vec<(u16, [u64; 17], u64, u64)> = Vec::new();
    for slot in 0..ROUTE_SLOTS {
        let mut buckets = [0u64; 17];
        let (mut nanos, mut bytes) = (0u64, 0u64);
        for shard in shards.iter() {
            let cells = &shard.routes[slot];
            for (total, cell) in buckets.iter_mut().zip(&cells.buckets) {
                *total += cell.load(Ordering::Relaxed);
            }
            nanos += cells.nanos.load(Ordering::Relaxed);
            bytes += cells.bytes.load(Ordering::Relaxed);
        }
        if buckets.iter().any(|&n| n > 0) {
            routes.push((slot as u16, buckets, nanos, bytes));
        }
    }
    family(
        out,
        "http_request_duration_seconds",
        "histogram",
        "Time from reading a request to having its response ready, by route.",
    );
    for (slot, buckets, nanos, _) in &routes {
        let labels = labels_text(&route_labels(*slot));
        let mut cumulative = 0;
        for (i, count) in buckets.iter().enumerate() {
            cumulative += count;
            let bound = if i < BUCKET_SECONDS.len() { format!("{}", BUCKET_SECONDS[i]) } else { "+Inf".to_string() };
            sample(out, "http_request_duration_seconds_bucket", &labels, Some(("le", &bound)), cumulative as f64);
        }
        sample(out, "http_request_duration_seconds_sum", &labels, None, *nanos as f64 / 1e9);
        sample(out, "http_request_duration_seconds_count", &labels, None, cumulative as f64);
    }
    family(out, "http_response_size_bytes", "summary", "Bytes in response bodies, by route.");
    for (slot, buckets, _, bytes) in &routes {
        let labels = labels_text(&route_labels(*slot));
        sample(out, "http_response_size_bytes_sum", &labels, None, *bytes as f64);
        sample(out, "http_response_size_bytes_count", &labels, None, buckets.iter().sum::<u64>() as f64);
    }
    family(out, "http_requests_in_flight", "gauge", "Requests being handled.");
    sample(out, "http_requests_in_flight", "", None, IN_FLIGHT.load(Ordering::Relaxed).max(0) as f64);
    family(out, "http_connections_open", "gauge", "Open connections, WebSockets included.");
    sample(out, "http_connections_open", "", None, crate::http::open_connections() as f64);
    family(out, "http_connections_total", "counter", "Connections accepted.");
    sample(out, "http_connections_total", "", None, CONNECTIONS.load(Ordering::Relaxed) as f64);
    family(
        out,
        "http_request_errors_total",
        "counter",
        "Requests that failed: a handler that panicked, a client too slow to send its request, a request that could not be parsed, or one too large.",
    );
    for (i, kind) in ERROR_KINDS.iter().enumerate() {
        sample(out, "http_request_errors_total", "", Some(("kind", kind)), ERRORS[i].load(Ordering::Relaxed) as f64);
    }
    let unrecorded = UNRECORDED.load(Ordering::Relaxed);
    if unrecorded > 0 {
        family(out, "runtime_unrecorded_requests_total", "counter", "Requests with no room left to count them.");
        sample(out, "runtime_unrecorded_requests_total", "", None, unrecorded as f64);
    }
}

fn write_program(out: &mut String) {
    let registry = REGISTRY.read().unwrap_or_else(|p| p.into_inner());
    for metric in registry.iter() {
        family(out, &metric.name, metric.kind.name(), &metric.help);
        let all = metric.series.read().unwrap_or_else(|p| p.into_inner());
        let mut keys: Vec<&Box<str>> = all.keys().collect();
        keys.sort();
        for key in keys {
            let series = &all[key];
            let pairs: Vec<(&str, &str)> =
                metric.labels.iter().map(String::as_str).zip(key.split_terminator('\0')).collect();
            let labels = labels_text(&pairs);
            let value = f64::from_bits(series.value.load(Ordering::Relaxed));
            if metric.kind != Kind::Histogram {
                sample(out, &metric.name, &labels, None, value);
                continue;
            }
            let mut cumulative = 0;
            for (i, count) in series.buckets.iter().enumerate() {
                cumulative += count.load(Ordering::Relaxed);
                let bound = metric.bounds.get(i).map_or("+Inf".to_string(), |b| format!("{b}"));
                sample(out, &format!("{}_bucket", metric.name), &labels, Some(("le", &bound)), cumulative as f64);
            }
            sample(out, &format!("{}_sum", metric.name), &labels, None, value);
            sample(out, &format!("{}_count", metric.name), &labels, None, cumulative as f64);
        }
    }
}

unsafe extern "C" {
    fn sysconf(name: i32) -> i64;
}

/// Seconds since the Unix epoch at which the process started, the CPU seconds it has used, and
/// its resident memory in bytes, from `/proc` (Linux).
pub(crate) fn process_figures() -> (Option<f64>, Option<f64>, Option<f64>) {
    const SC_CLK_TCK: i32 = 2;
    const SC_PAGESIZE: i32 = 30;
    // SAFETY: plain queries.
    let (ticks, page) = unsafe { (sysconf(SC_CLK_TCK), sysconf(SC_PAGESIZE)) };
    let stat = std::fs::read_to_string("/proc/self/stat").ok();
    // Fields after the command name, which is in parentheses and may hold spaces: the state is
    // field 3, so field n is at n - 3.
    let fields: Vec<&str> = stat
        .as_deref()
        .and_then(|s| s.rsplit_once(')'))
        .map_or(Vec::new(), |(_, rest)| rest.split_whitespace().collect());
    let field = |n: usize| fields.get(n - 3).and_then(|f| f.parse::<f64>().ok());
    let boot = std::fs::read_to_string("/proc/stat").ok().and_then(|s| {
        s.lines().find_map(|line| line.strip_prefix("btime ").and_then(|t| t.trim().parse::<f64>().ok()))
    });
    let ticks = (ticks > 0).then_some(ticks as f64);
    let start = boot.zip(field(22)).zip(ticks).map(|((boot, at), ticks)| boot + at / ticks);
    let cpu = field(14).zip(field(15)).zip(ticks).map(|((user, system), ticks)| (user + system) / ticks);
    let resident = std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| s.split_whitespace().nth(1).and_then(|pages| pages.parse::<f64>().ok()))
        .map(|pages| pages * page.max(1) as f64);
    (start, cpu, resident)
}

fn write_process(out: &mut String) {
    let (start, cpu, resident) = process_figures();
    if let Some(start) = start {
        family(
            out,
            "process_start_time_seconds",
            "gauge",
            "When the process started, in seconds since the Unix epoch.",
        );
        sample(out, "process_start_time_seconds", "", None, start.round());
    }
    if let Some(cpu) = cpu {
        family(out, "process_cpu_seconds_total", "counter", "CPU time used, user and system, in seconds.");
        sample(out, "process_cpu_seconds_total", "", None, cpu);
    }
    if let Some(resident) = resident {
        family(out, "process_resident_memory_bytes", "gauge", "Resident memory in bytes.");
        sample(out, "process_resident_memory_bytes", "", None, resident);
    }
    let (heap, collections) = crate::gc::heap_stats();
    family(out, "runtime_heap_bytes", "gauge", "The size of the collected heap in bytes.");
    sample(out, "runtime_heap_bytes", "", None, heap as f64);
    family(out, "runtime_gc_collections_total", "counter", "Collections the garbage collector has run.");
    sample(out, "runtime_gc_collections_total", "", None, collections as f64);
    family(out, "runtime_tasks", "gauge", "Tasks running or waiting, one per open connection among them.");
    sample(out, "runtime_tasks", "", None, crate::fiber::live_tasks() as f64);
    family(out, "runtime_worker_threads", "gauge", "Threads that run tasks.");
    sample(out, "runtime_worker_threads", "", None, crate::fiber::worker_threads() as f64);
    if crate::trace::on() {
        family(out, "trace_spans_exported_total", "counter", "Spans the trace collector has taken.");
        sample(out, "trace_spans_exported_total", "", None, crate::trace::exported() as f64);
        family(
            out,
            "trace_spans_dropped_total",
            "counter",
            "Spans dropped: the queue was full, or the collector did not take them.",
        );
        sample(out, "trace_spans_dropped_total", "", None, crate::trace::dropped() as f64);
    }
    family(out, "build_info", "gauge", "The version of the language the program was built with.");
    sample(out, "build_info", "", Some(("version", env!("CARGO_PKG_VERSION"))), 1.0);
}

/// Every metric, in the Prometheus text format: the HTTP server's (once recording is on), the
/// program's, and the process's.
pub(crate) fn exposition() -> String {
    let mut out = String::with_capacity(16 * 1024);
    if on() {
        write_http(&mut out);
    }
    write_program(&mut out);
    write_process(&mut out);
    out
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_metrics_text() -> Str {
    Str::from_bytes(exposition().as_bytes())
}

/// Every sample with a finite value: its name and labels as the text format writes them, then
/// the value, each followed by a NUL.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_metrics_snapshot() -> Str {
    let text = exposition();
    let mut out = String::new();
    for line in text.lines().filter(|l| !l.starts_with('#') && !l.is_empty()) {
        let Some((series, value)) = line.rsplit_once(' ') else { continue };
        let Ok(number) = value.parse::<f64>() else { continue };
        if number.is_finite() {
            out.push_str(series);
            out.push('\0');
            out.push_str(value);
            out.push('\0');
        }
    }
    Str::from_bytes(out.as_bytes())
}

// ---- Basic authentication -------------------------------------------------------------------

/// A user name and password, kept only as HMAC-SHA256 digests under a key made for this
/// process, so that comparing them takes the same time whatever their lengths and contents.
pub(crate) struct Credentials {
    user: [u8; 32],
    password: Password,
}

enum Password {
    /// The digest of the password.
    Plain([u8; 32]),
    /// A PBKDF2-SHA256 hash (`METRICS_PASSWORD_HASH`), and the digest of the last password
    /// that matched it, so that a dashboard polling every few seconds pays for PBKDF2 once.
    Hashed { iterations: std::num::NonZeroU32, salt: Vec<u8>, hash: Vec<u8>, verified: Mutex<Option<[u8; 32]>> },
}

fn hmac_key() -> &'static ring::hmac::Key {
    static KEY: OnceLock<ring::hmac::Key> = OnceLock::new();
    KEY.get_or_init(|| {
        ring::hmac::Key::generate(ring::hmac::HMAC_SHA256, &ring::rand::SystemRandom::new())
            .unwrap_or_else(|_| crate::panic::panic_at("the operating system gave no random bytes", ""))
    })
}

fn digest(text: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(ring::hmac::sign(hmac_key(), text).as_ref());
    out
}

impl Credentials {
    fn new(user: &str, password: &str) -> Credentials {
        Credentials { user: digest(user.as_bytes()), password: Password::Plain(digest(password.as_bytes())) }
    }

    /// Credentials whose password is known only by its hash, written
    /// `pbkdf2-sha256$<iterations>$<salt, base64>$<hash, base64>`; `None` if it is not one.
    fn hashed(user: &str, text: &str) -> Option<Credentials> {
        let mut parts = text.trim().split('$');
        if parts.next()? != "pbkdf2-sha256" {
            return None;
        }
        let iterations = parts.next()?.parse::<u32>().ok().and_then(std::num::NonZeroU32::new)?;
        let salt = crate::crypto::base64_decode(parts.next()?.as_bytes())?;
        let hash = crate::crypto::base64_decode(parts.next()?.as_bytes())?;
        if parts.next().is_some() || salt.is_empty() || hash.len() < 16 {
            return None;
        }
        let password = Password::Hashed { iterations, salt, hash, verified: Mutex::new(None) };
        Some(Credentials { user: digest(user.as_bytes()), password })
    }

    /// Whether an `Authorization` header carries these credentials.
    fn accept(&self, header: &str) -> bool {
        let header = header.trim();
        let Some((scheme, encoded)) = header.split_once(' ') else { return false };
        if !scheme.eq_ignore_ascii_case("basic") {
            return false;
        }
        let Some(decoded) = crate::crypto::base64_decode(encoded.trim().as_bytes()) else { return false };
        let Some(colon) = decoded.iter().position(|&b| b == b':') else { return false };
        // Both halves are compared, whatever the first gives.
        let user = crate::crypto::equal(&digest(&decoded[..colon]), &self.user);
        let given = &decoded[colon + 1..];
        let password = match &self.password {
            Password::Plain(password) => crate::crypto::equal(&digest(given), password),
            Password::Hashed { iterations, salt, hash, verified } => {
                let given_digest = digest(given);
                let known = *verified.lock().unwrap_or_else(|p| p.into_inner());
                if known.is_some_and(|known| crate::crypto::equal(&given_digest, &known)) {
                    true
                } else if ring::pbkdf2::verify(ring::pbkdf2::PBKDF2_HMAC_SHA256, *iterations, salt, given, hash).is_ok()
                {
                    *verified.lock().unwrap_or_else(|p| p.into_inner()) = Some(given_digest);
                    true
                } else {
                    false
                }
            }
        };
        user & password
    }
}

static CREDENTIALS: RwLock<Vec<std::sync::Arc<Credentials>>> = RwLock::new(Vec::new());

/// Keep a user name and password for [`lrt_metrics_authorized`]; returns their id. With
/// `hashed` 1, `password` is a hash from `hash-password`; -1 if it is not one.
///
/// # Safety
/// The pointer and length pairs describe UTF-8 strings.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_metrics_credentials(
    user: *const u8,
    user_len: i64,
    password: *const u8,
    password_len: i64,
    hashed: i64,
) -> i64 {
    let (user, password) = unsafe { (Str::from_raw(user, user_len), Str::from_raw(password, password_len)) };
    let credentials =
        if hashed == 1 { Credentials::hashed(user, password) } else { Some(Credentials::new(user, password)) };
    let Some(credentials) = credentials else {
        crate::sys::record_error(format!(
            "METRICS_PASSWORD_HASH is not a password hash: make one with `{} hash-password`, which prints pbkdf2-sha256$<iterations>$<salt>$<hash>",
            lang_names::CLI_NAME
        ));
        return -1;
    };
    let mut all = CREDENTIALS.write().unwrap_or_else(|p| p.into_inner());
    all.push(std::sync::Arc::new(credentials));
    all.len() as i64 - 1
}

// ---- failed sign-ins ------------------------------------------------------------------------

/// Failed sign-ins: a client has [`MAX_FAILURES`], refilled at that many a minute (docs/14,
/// milestone 14). The failure that uses the last locks the client out for [`LOCK_OUT`]
/// seconds; after that it has one try, and all ten again a minute later.
const MAX_FAILURES: u64 = 10;
const LOCK_OUT: u64 = 60;

fn sign_ins() -> &'static crate::ratelimit::Limiter {
    static LIMITER: OnceLock<crate::ratelimit::Limiter> = OnceLock::new();
    LIMITER.get_or_init(|| crate::ratelimit::Limiter::new(LOCK_OUT * 1_000_000_000 / MAX_FAILURES, MAX_FAILURES))
}

/// Seconds `client` is still locked out for, if it is.
fn locked(client: &str, now: i64) -> Option<u64> {
    sign_ins().waiting(client, now)
}

/// Count a failed sign-in from `client`; true when it has just been locked out.
fn failed(client: &str, now: i64) -> bool {
    match sign_ins().take(client, now) {
        crate::ratelimit::Decision::Allowed { available: 0, .. } => {
            sign_ins().lock_out(client, now);
            true
        }
        _ => false,
    }
}

static WARNED_PLAIN_HTTP: AtomicBool = AtomicBool::new(false);

/// Whether an `Authorization` header from `client` carries the credentials with this id: 1 if
/// it does, 0 if not, and minus the seconds left when the client is locked out after
/// [`MAX_FAILURES`] failures in a minute. `handle` is the request (negative for a test
/// request), for the warning about credentials sent over plain HTTP.
///
/// # Safety
/// The pointer and length pairs describe UTF-8 strings; `handle` is a live request or negative.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_metrics_authorized(
    id: i64,
    header: *const u8,
    header_len: i64,
    client: *const u8,
    client_len: i64,
    handle: i64,
) -> i64 {
    let (header, client) = unsafe { (Str::from_raw(header, header_len), Str::from_raw(client, client_len)) };
    let now = crate::ratelimit::now();
    if let Some(seconds) = locked(client, now) {
        return -(seconds.max(1) as i64);
    }
    if !header.is_empty()
        && handle >= 0
        && !WARNED_PLAIN_HTTP.load(Ordering::Relaxed)
        && !unsafe { crate::http::request_is_private(handle) }
        && !WARNED_PLAIN_HTTP.swap(true, Ordering::Relaxed)
    {
        crate::log::write_line(
            "warn",
            "metrics credentials arrived over plain HTTP, where anyone on the way can read them: serve the metrics over HTTPS, behind a proxy that ends TLS (with trusted_proxies and X-Forwarded-Proto), or on a private network",
            "",
        );
    }
    let credentials = CREDENTIALS.read().unwrap_or_else(|p| p.into_inner()).get(id.max(0) as usize).cloned();
    if credentials.is_some_and(|c| c.accept(header)) {
        return 1;
    }
    // A request without credentials is how a browser learns that it must ask for them; only
    // wrong ones count against the client.
    if !header.is_empty() && failed(client, now) {
        crate::log::write_line(
            "warn",
            &format!(
                "{MAX_FAILURES} failed sign-ins to the metrics from {client} within a minute; refusing it for {LOCK_OUT} s"
            ),
            "",
        );
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn basic(user: &str, password: &str) -> String {
        let text = format!("{user}:{password}");
        let mut encoded = String::new();
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        for chunk in text.as_bytes().chunks(3) {
            let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            for i in 0..4 {
                if i <= chunk.len() {
                    encoded.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
                } else {
                    encoded.push('=');
                }
            }
        }
        format!("Basic {encoded}")
    }

    #[test]
    fn credentials_are_kept_and_compared_as_hmac_digests() {
        let credentials = Credentials::new("admin", "s3cret");
        // What is kept is the keyed digest, never the text, and it is the same length for any
        // password, so the comparison's time cannot depend on the length of a guess.
        let Password::Plain(password) = credentials.password else { panic!("not plain") };
        assert_eq!(password, digest(b"s3cret"));
        assert_ne!(&password[..6], b"s3cret");
        assert_eq!(credentials.user.len(), digest(b"a much longer user name than admin").len());
        assert_ne!(digest(b"s3cret"), digest(b"s3cre"));
        assert!(credentials.accept(&basic("admin", "s3cret")));
        assert!(credentials.accept(&format!("  bAsIc {}", &basic("admin", "s3cret")[6..])));
        for (user, password) in [("admin", "s3cre"), ("admin", "s3cret!"), ("admi", "s3cret"), ("", ""), ("admin", "")]
        {
            assert!(!credentials.accept(&basic(user, password)), "{user}:{password}");
        }
        assert!(!credentials.accept("Bearer abc"));
        assert!(!credentials.accept("Basic !!!"));
        assert!(!credentials.accept("Basic YWRtaW4="));
        // A colon in the password is part of it.
        assert!(Credentials::new("a", "b:c").accept(&basic("a", "b:c")));
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn derive(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
        let mut out = [0u8; 32];
        let rounds = std::num::NonZeroU32::new(iterations).unwrap();
        ring::pbkdf2::derive(ring::pbkdf2::PBKDF2_HMAC_SHA256, rounds, salt, password, &mut out);
        out
    }

    #[test]
    fn password_hashes_are_pbkdf2_sha256() {
        // RFC 6070's inputs, with the published results for SHA-256.
        assert_eq!(
            hex(&derive(b"password", b"salt", 1)),
            "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b"
        );
        assert_eq!(
            hex(&derive(b"password", b"salt", 2)),
            "ae4d0c95af6b46d32d0adff928f06dd02a303f8ef3c251dfd6e2d85a95474c43"
        );
        assert_eq!(
            hex(&derive(b"password", b"salt", 4096)),
            "c5e478d59288c841aa530db6845c4c8d962893a001ce4e11a4963873aa98134a"
        );
        use crate::crypto::base64_encode;
        let text = format!(
            "pbkdf2-sha256$4096${}${}",
            base64_encode(b"salt"),
            base64_encode(&derive(b"password", b"salt", 4096))
        );
        let credentials = Credentials::hashed("admin", &text).unwrap();
        assert!(credentials.accept(&basic("admin", "password")));
        // The second time, from the cache of the last password that matched.
        assert!(credentials.accept(&basic("admin", "password")));
        assert!(!credentials.accept(&basic("admin", "Password")));
        assert!(!credentials.accept(&basic("root", "password")));
        let Password::Hashed { verified, .. } = &credentials.password else { panic!("not hashed") };
        assert_eq!(*verified.lock().unwrap(), Some(digest(b"password")));
        for bad in [
            "",
            "pbkdf2-sha256$0$c2FsdA==$Eg+2z/z4syxD5yJSVsT4N6hlSMkszDVICAWYfLcL4Xs=",
            "pbkdf2-sha1$1$c2FsdA==$Eg+2z/z4syxD5yJSVsT4N6hlSMkszDVICAWYfLcL4Xs=",
            "pbkdf2-sha256$1$!!$Eg+2z/z4syxD5yJSVsT4N6hlSMkszDVICAWYfLcL4Xs=",
            "pbkdf2-sha256$1$c2FsdA==$EgA=",
            "pbkdf2-sha256$1$c2FsdA==$Eg+2z/z4syxD5yJSVsT4N6hlSMkszDVICAWYfLcL4Xs=$x",
            "s3cret",
        ] {
            assert!(Credentials::hashed("admin", bad).is_none(), "{bad}");
        }
        assert!(
            Credentials::hashed("admin", "pbkdf2-sha256$1$c2FsdA==$Eg+2z/z4syxD5yJSVsT4N6hlSMkszDVICAWYfLcL4Xs=")
                .is_some()
        );
    }

    #[test]
    fn ten_failures_in_a_minute_lock_a_client_out() {
        const SECOND: i64 = 1_000_000_000;
        let now = 1000 * SECOND;
        let client = "192.0.2.77";
        for _ in 0..MAX_FAILURES - 1 {
            assert!(!failed(client, now));
        }
        assert_eq!(locked(client, now), None);
        assert!(failed(client, now), "the tenth failure locks the client out");
        assert_eq!(locked(client, now), Some(60));
        assert_eq!(locked(client, now + 30 * SECOND), Some(30));
        assert_eq!(locked("192.0.2.78", now), None, "other clients are not affected");
        let later = now + 61 * SECOND;
        assert_eq!(locked(client, later), None);
        // A minute after the lock-out the client has one try; another failure locks it again.
        assert!(failed(client, later));
        // Two minutes after, it has all ten again.
        let much_later = later + 120 * SECOND;
        for _ in 0..MAX_FAILURES - 1 {
            assert!(!failed(client, much_later));
        }
        // Failures spread over more than a minute never add up to ten.
        let slow = "192.0.2.79";
        for i in 0..30 {
            assert!(!failed(slow, now + 7 * i * SECOND));
        }
    }

    #[test]
    fn names_are_checked() {
        assert!(valid_metric_name("jobs_done_total") && valid_metric_name("ns:sub_total") && valid_metric_name("_x"));
        assert!(!valid_metric_name("2fast") && !valid_metric_name("a-b") && !valid_metric_name(""));
        assert!(valid_label_name("queue") && !valid_label_name("__name") && !valid_label_name("a:b"));
        let message = register(Kind::Counter, "http_requests_total", "", vec![], vec![]).unwrap_err();
        assert!(message.contains("runtime's own"), "{message}");
        assert!(register(Kind::Histogram, "unit_h", "", vec!["le".into()], vec![]).is_err());
        assert!(register(Kind::Histogram, "unit_h", "", vec![], vec![2.0, 1.0]).is_err());
    }

    #[test]
    fn registering_again_finds_the_same_metric() {
        let a = register(Kind::Counter, "unit_again_total", "help", vec!["x".into()], vec![]).unwrap();
        let b = register(Kind::Counter, "unit_again_total", "other help", vec!["x".into()], vec![]).unwrap();
        assert_eq!(a, b);
        let message = register(Kind::Gauge, "unit_again_total", "", vec!["x".into()], vec![]).unwrap_err();
        assert!(message.contains("already registered as a counter"), "{message}");
    }

    #[test]
    fn updates_and_the_text_format() {
        let counter =
            register(Kind::Counter, "unit_jobs_total", "Jobs \\ done\nfast", vec!["queue".into()], vec![]).unwrap();
        update(counter, "email\0", Update::Add(1.0)).unwrap();
        update(counter, "email\0", Update::Add(2.5)).unwrap();
        update(counter, "a \"b\"\\\n\0", Update::Add(1.0)).unwrap();
        assert!(update(counter, "email\0", Update::Add(-1.0)).unwrap_err().contains("only goes up"));
        assert!(update(counter, "", Update::Add(1.0)).unwrap_err().contains("takes the labels"));
        assert!(update(counter, "email\0", Update::Set(1.0)).is_err());
        let gauge = register(Kind::Gauge, "unit_depth", "Depth", vec![], vec![]).unwrap();
        update(gauge, "", Update::Set(7.0)).unwrap();
        update(gauge, "", Update::Add(-2.0)).unwrap();
        let histogram = register(Kind::Histogram, "unit_seconds", "Time", vec![], vec![0.5, 1.0]).unwrap();
        for value in [0.1, 0.5, 0.7, 3.0] {
            update(histogram, "", Update::Observe(value)).unwrap();
        }
        let mut out = String::new();
        write_program(&mut out);
        assert!(
            out.contains("# HELP unit_jobs_total Jobs \\\\ done\\nfast\n# TYPE unit_jobs_total counter\n"),
            "{out}"
        );
        assert!(out.contains("unit_jobs_total{queue=\"email\"} 3.5\n"), "{out}");
        assert!(out.contains("unit_jobs_total{queue=\"a \\\"b\\\"\\\\\\n\"} 1\n"), "{out}");
        assert!(out.contains("# TYPE unit_depth gauge\nunit_depth 5\n"), "{out}");
        for line in [
            "unit_seconds_bucket{le=\"0.5\"} 2",
            "unit_seconds_bucket{le=\"1\"} 3",
            "unit_seconds_bucket{le=\"+Inf\"} 4",
            "unit_seconds_sum 4.3",
            "unit_seconds_count 4",
        ] {
            assert!(out.contains(&format!("{line}\n")), "{line} in {out}");
        }
    }

    #[test]
    fn label_values_beyond_the_cap_share_one_series() {
        let id = register(Kind::Counter, "unit_capped_total", "", vec!["user".into()], vec![]).unwrap();
        for i in 0..MAX_LABEL_VALUES + 50 {
            update(id, &format!("u{i}\0"), Update::Add(1.0)).unwrap();
        }
        update(id, "u3\0", Update::Add(1.0)).unwrap();
        let registry = REGISTRY.read().unwrap();
        let series = registry[id].series.read().unwrap();
        assert_eq!(series.len(), MAX_LABEL_VALUES + 1);
        assert_eq!(f64::from_bits(series["other\0"].value.load(Ordering::Relaxed)), 50.0);
        assert_eq!(f64::from_bits(series["u3\0"].value.load(Ordering::Relaxed)), 2.0);
    }

    #[test]
    fn series_keys_round_trip() {
        for (route, method, status) in [(0, 0, 200), (UNMATCHED, 7, 404), (OTHER_ROUTE, 3, 999), (17, 5, 0)] {
            assert_eq!(key_parts(series_key(route, method, status)), (route, method, status));
        }
        assert_eq!(method_index("PATCH"), 4);
        assert_eq!(method_index("PROPFIND"), 7);
    }
}
