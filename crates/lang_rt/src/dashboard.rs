//! The metrics dashboard (docs/14-production-web-plan.md, 4.8): its page, style sheet and
//! script, the history it charts, and the recent server errors and slowest requests.
//!
//! The files are reached only through [`lrt_dashboard_asset`], which only `server.metrics`
//! calls, so `--gc-sections` drops them from programs that serve no metrics.
//!
//! **History.** [`lrt_dashboard_start`] starts a thread that adds a [`Sample`] to a ring of
//! [`SAMPLES`] every interval (5 s, so an hour): what changed in the request counters since the
//! last sample, and the gauges as they stand. The page asks for the samples after the last one
//! it has (`data.json?since=`), so polling downloads only what is new.
//!
//! **Events.** Once the dashboard is on, every answered request is compared with two cheap
//! thresholds: a status of 500 or more, and a duration above the fastest of the slowest
//! [`SLOWEST_KEPT`] of the last five minutes. Only a request that passes one takes a lock and
//! allocates. Paths are kept without their query strings, which often carry tokens.

use crate::Str;
use crate::metrics::{self, BUCKET_SECONDS, RouteTotal};
use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The page template, its style sheet and its script, in the order the `http` module numbers
/// them.
static ASSETS: [&str; 3] = [
    include_str!("../assets/dashboard/index.html"),
    include_str!("../assets/dashboard/dashboard.css"),
    include_str!("../assets/dashboard/dashboard.js"),
];

/// One of the dashboard's files, as a string that needs no copy; empty for an unknown index.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_dashboard_asset(index: i64) -> Str {
    match usize::try_from(index).ok().and_then(|i| ASSETS.get(i)) {
        Some(text) => Str { ptr: text.as_ptr(), len: text.len() as i64 },
        None => Str::empty(),
    }
}

/// Samples kept: an hour at the default interval.
const SAMPLES: usize = 720;
/// The default time between samples.
const INTERVAL: Duration = Duration::from_secs(5);
/// Samples that make up "the last minute" at the default interval.
const RECENT: usize = 12;
const ERRORS_KEPT: usize = 50;
const SLOWEST_KEPT: usize = 20;
const SLOWEST_WINDOW: Duration = Duration::from_secs(300);

/// What changed during one interval, and the gauges at its end.
struct Sample {
    seq: u64,
    time: f64,
    /// Requests answered, by status class (1xx to 5xx).
    classes: [u64; 5],
    /// Their durations, by bucket.
    buckets: [u64; 17],
    in_flight: i64,
    connections: u64,
    heap: u64,
    resident: u64,
    tasks: u64,
    /// Requests by route slot, for the slots that had any.
    routes: Vec<(u16, u64)>,
    /// The program's metrics, by registry id.
    program: Vec<f64>,
}

struct History {
    interval: Duration,
    next_seq: u64,
    samples: VecDeque<Sample>,
    previous: HashMap<u16, RouteTotal>,
}

static HISTORY: Mutex<Option<History>> = Mutex::new(None);
static STARTED: OnceLock<()> = OnceLock::new();

/// A request kept for the page: a server error, or one of the slowest.
struct Event {
    time: f64,
    at: Instant,
    nanos: Option<u64>,
    method: String,
    route: String,
    path: String,
    status: i64,
    id: String,
}

static EVENTS_ON: AtomicBool = AtomicBool::new(false);
/// The duration a request must exceed to be one of the slowest: 0 while fewer than
/// [`SLOWEST_KEPT`] are kept.
static SLOW_FLOOR: AtomicU64 = AtomicU64::new(0);
static ERRORS: Mutex<VecDeque<Event>> = Mutex::new(VecDeque::new());
static SLOWEST: Mutex<Vec<Event>> = Mutex::new(Vec::new());

fn unix_now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

/// Start keeping history and events, once per process. The interval is 5 s, or the
/// milliseconds in the environment variable `<PREFIX>METRICS_INTERVAL_MS` (for tests).
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_dashboard_start() {
    STARTED.get_or_init(|| {
        let interval = std::env::var(lang_names::env_var("METRICS_INTERVAL_MS"))
            .ok()
            .and_then(|ms| ms.trim().parse::<u64>().ok())
            .map_or(INTERVAL, |ms| Duration::from_millis(ms.max(50)));
        let previous = metrics::route_totals().into_iter().map(|t| (t.slot, t)).collect();
        *HISTORY.lock().unwrap_or_else(|p| p.into_inner()) =
            Some(History { interval, next_seq: 1, samples: VecDeque::with_capacity(SAMPLES), previous });
        EVENTS_ON.store(true, Ordering::SeqCst);
        let started =
            std::thread::Builder::new().name("metrics-history".into()).stack_size(256 * 1024).spawn(move || {
                loop {
                    std::thread::sleep(interval);
                    sample();
                }
            });
        if started.is_err() {
            crate::log::write_line("warn", "could not start the metrics dashboard's history thread", "");
        }
    });
}

fn sample() {
    let totals = metrics::route_totals();
    let (_, _, resident) = metrics::process_figures();
    let (heap, _) = crate::gc::heap_stats();
    let program = metrics::program_values().into_iter().map(|(_, _, _, value)| value).collect();
    let mut guard = HISTORY.lock().unwrap_or_else(|p| p.into_inner());
    let Some(history) = guard.as_mut() else { return };
    let mut classes = [0u64; 5];
    let mut buckets = [0u64; 17];
    let mut routes = Vec::new();
    for total in &totals {
        let before = history.previous.get(&total.slot).copied().unwrap_or_default();
        for (sum, (now, then)) in classes.iter_mut().zip(total.by_class.iter().zip(before.by_class)) {
            *sum += now.saturating_sub(then);
        }
        for (sum, (now, then)) in buckets.iter_mut().zip(total.buckets.iter().zip(before.buckets)) {
            *sum += now.saturating_sub(then);
        }
        let requests = total.requests.saturating_sub(before.requests);
        if requests > 0 {
            routes.push((total.slot, requests));
        }
    }
    history.previous = totals.into_iter().map(|t| (t.slot, t)).collect();
    let seq = history.next_seq;
    history.next_seq += 1;
    history.samples.push_back(Sample {
        seq,
        time: unix_now(),
        classes,
        buckets,
        in_flight: metrics::in_flight(),
        connections: crate::http::open_connections() as u64,
        heap: heap as u64,
        resident: resident.unwrap_or(0.0) as u64,
        tasks: crate::fiber::live_tasks() as u64,
        routes,
        program,
    });
    if history.samples.len() > SAMPLES {
        history.samples.pop_front();
    }
    drop(guard);
    let mut slowest = SLOWEST.lock().unwrap_or_else(|p| p.into_inner());
    prune_slowest(&mut slowest, Instant::now());
}

// ---- events ---------------------------------------------------------------------------------

/// Whether answered requests are compared with the thresholds.
#[inline]
pub(crate) fn events_on() -> bool {
    EVENTS_ON.load(Ordering::Relaxed)
}

/// Whether a request with this status and duration is kept.
#[inline]
pub(crate) fn qualifies(status: i64, nanos: Option<u64>) -> bool {
    status >= 500 || nanos.is_some_and(|n| n > SLOW_FLOOR.load(Ordering::Relaxed))
}

fn prune_slowest(slowest: &mut Vec<Event>, now: Instant) {
    slowest.retain(|e| now.duration_since(e.at) < SLOWEST_WINDOW);
    let floor =
        if slowest.len() >= SLOWEST_KEPT { slowest.iter().filter_map(|e| e.nanos).min().unwrap_or(0) } else { 0 };
    SLOW_FLOOR.store(floor, Ordering::Relaxed);
}

/// Keep a request that [`qualifies`]. `path` has no query string.
pub(crate) fn note(method: &str, route: u16, path: &str, status: i64, nanos: Option<u64>, id: String) {
    let (_, pattern) = metrics::route_name(route);
    let at = Instant::now();
    let event = |id: String| Event {
        time: unix_now(),
        at,
        nanos,
        method: method.to_string(),
        route: pattern.to_string(),
        path: path.chars().take(512).collect(),
        status,
        id,
    };
    if status >= 500 {
        let mut errors = ERRORS.lock().unwrap_or_else(|p| p.into_inner());
        errors.push_front(event(id.clone()));
        errors.truncate(ERRORS_KEPT);
    }
    if nanos.is_some_and(|n| n > SLOW_FLOOR.load(Ordering::Relaxed)) {
        let mut slowest = SLOWEST.lock().unwrap_or_else(|p| p.into_inner());
        slowest.push(event(id));
        slowest.sort_by_key(|a| std::cmp::Reverse(a.nanos));
        slowest.truncate(SLOWEST_KEPT);
        prune_slowest(&mut slowest, at);
    }
}

// ---- data.json ------------------------------------------------------------------------------

/// The `q` quantile of durations counted in the request buckets, in seconds, interpolating
/// linearly within a bucket as Prometheus' `histogram_quantile` does; `None` with no requests.
pub(crate) fn quantile(q: f64, buckets: &[u64; 17]) -> Option<f64> {
    let total: u64 = buckets.iter().sum();
    if total == 0 {
        return None;
    }
    let rank = q * total as f64;
    let mut below = 0u64;
    for (i, &count) in buckets.iter().enumerate() {
        if count > 0 && (below + count) as f64 >= rank {
            let Some(&upper) = BUCKET_SECONDS.get(i) else { return Some(BUCKET_SECONDS[15]) };
            let lower = if i == 0 { 0.0 } else { BUCKET_SECONDS[i - 1] };
            return Some(lower + (upper - lower) * ((rank - below as f64) / count as f64).clamp(0.0, 1.0));
        }
        below += count;
    }
    Some(BUCKET_SECONDS[15])
}

fn push_string(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // Escaped so that the JSON can never close a <script> or start a comment in HTML.
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '&' => out.push_str("\\u0026"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn push_number(out: &mut String, value: Option<f64>) {
    match value {
        Some(v) if v.is_finite() => {
            let _ = write!(out, "{v}");
        }
        _ => out.push_str("null"),
    }
}

fn push_events(out: &mut String, events: &[&Event]) {
    out.push('[');
    for (i, e) in events.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"time\":");
        push_number(out, Some(e.time.round()));
        out.push_str(",\"method\":");
        push_string(out, &e.method);
        out.push_str(",\"route\":");
        push_string(out, &e.route);
        out.push_str(",\"path\":");
        push_string(out, &e.path);
        let _ = write!(out, ",\"status\":{}", e.status);
        out.push_str(",\"seconds\":");
        push_number(out, e.nanos.map(|n| n as f64 / 1e9));
        out.push_str(",\"id\":");
        push_string(out, &e.id);
        out.push('}');
    }
    out.push(']');
}

/// Everything the page shows: the samples after `since`, the totals and the events.
pub(crate) fn data(since: i64) -> String {
    let mut out = String::with_capacity(8 * 1024);
    let totals = metrics::route_totals();
    let program = metrics::program_values();
    let (start, _, _) = metrics::process_figures();
    let guard = HISTORY.lock().unwrap_or_else(|p| p.into_inner());
    let (interval, samples): (f64, Vec<&Sample>) = match guard.as_ref() {
        Some(h) => (h.interval.as_secs_f64(), h.samples.iter().collect()),
        None => (INTERVAL.as_secs_f64(), Vec::new()),
    };
    let last = samples.last().map_or(0, |s| s.seq);
    out.push_str("{\"version\":");
    push_string(&mut out, env!("CARGO_PKG_VERSION"));
    out.push_str(",\"started\":");
    push_number(&mut out, start.map(f64::round));
    out.push_str(",\"now\":");
    push_number(&mut out, Some(unix_now()));
    out.push_str(",\"interval\":");
    push_number(&mut out, Some(interval));
    let _ = write!(out, ",\"last\":{last}");

    out.push_str(",\"samples\":[");
    for (i, s) in samples.iter().filter(|s| s.seq as i64 > since).enumerate() {
        if i > 0 {
            out.push(',');
        }
        let _ = write!(out, "{{\"seq\":{},\"time\":", s.seq);
        push_number(&mut out, Some((s.time * 1000.0).round() / 1000.0));
        let c = s.classes;
        let _ = write!(out, ",\"classes\":[{},{},{},{},{}]", c[0], c[1], c[2], c[3], c[4]);
        for (name, q) in [("p50", 0.5), ("p90", 0.9), ("p99", 0.99)] {
            let _ = write!(out, ",\"{name}\":");
            push_number(&mut out, quantile(q, &s.buckets));
        }
        let _ = write!(
            out,
            ",\"in_flight\":{},\"connections\":{},\"heap\":{},\"resident\":{},\"tasks\":{},\"program\":[",
            s.in_flight, s.connections, s.heap, s.resident, s.tasks
        );
        for (j, v) in s.program.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            push_number(&mut out, Some(*v));
        }
        out.push_str("]}");
    }
    out.push(']');

    // The last minute: the most recent samples.
    let recent = &samples[samples.len().saturating_sub(RECENT)..];
    let window = recent.len() as f64 * interval;
    let mut buckets = [0u64; 17];
    let mut classes = [0u64; 5];
    let mut route_recent: HashMap<u16, u64> = HashMap::new();
    for s in recent {
        for (sum, n) in buckets.iter_mut().zip(s.buckets) {
            *sum += n;
        }
        for (sum, n) in classes.iter_mut().zip(s.classes) {
            *sum += n;
        }
        for &(slot, n) in &s.routes {
            *route_recent.entry(slot).or_default() += n;
        }
    }
    let answered: u64 = classes.iter().sum();
    let all_time: u64 = totals.iter().map(|t| t.requests).sum();
    out.push_str(",\"summary\":{\"requests\":");
    let _ = write!(out, "{all_time}");
    out.push_str(",\"rate\":");
    push_number(&mut out, (window > 0.0).then(|| answered as f64 / window));
    out.push_str(",\"errors\":");
    push_number(&mut out, (answered > 0).then(|| classes[4] as f64 / answered as f64));
    for (name, q) in [("p50", 0.5), ("p95", 0.95), ("p99", 0.99)] {
        let _ = write!(out, ",\"{name}\":");
        push_number(&mut out, quantile(q, &buckets));
    }
    let _ =
        write!(out, ",\"in_flight\":{},\"connections\":{}}}", metrics::in_flight(), crate::http::open_connections());

    out.push_str(",\"routes\":[");
    for (i, t) in totals.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let (method, pattern) = metrics::route_name(t.slot);
        out.push_str("{\"method\":");
        push_string(&mut out, method);
        out.push_str(",\"route\":");
        push_string(&mut out, pattern);
        let _ = write!(out, ",\"requests\":{}", t.requests);
        out.push_str(",\"rate\":");
        push_number(&mut out, (window > 0.0).then(|| route_recent.get(&t.slot).copied().unwrap_or(0) as f64 / window));
        let share = |n: u64| (t.requests > 0).then(|| n as f64 / t.requests as f64);
        out.push_str(",\"client_errors\":");
        push_number(&mut out, share(t.by_class[3]));
        out.push_str(",\"server_errors\":");
        push_number(&mut out, share(t.by_class[4]));
        for (name, q) in [("p50", 0.5), ("p95", 0.95), ("p99", 0.99)] {
            let _ = write!(out, ",\"{name}\":");
            push_number(&mut out, quantile(q, &t.buckets));
        }
        out.push('}');
    }
    out.push(']');
    drop(guard);

    out.push_str(",\"program\":[");
    for (i, (name, kind, help, value)) in program.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"name\":");
        push_string(&mut out, name);
        out.push_str(",\"kind\":");
        push_string(&mut out, kind);
        out.push_str(",\"help\":");
        push_string(&mut out, help);
        out.push_str(",\"value\":");
        push_number(&mut out, Some(*value));
        out.push('}');
    }
    out.push(']');

    out.push_str(",\"errors\":");
    let errors = ERRORS.lock().unwrap_or_else(|p| p.into_inner());
    push_events(&mut out, &errors.iter().collect::<Vec<_>>());
    drop(errors);
    out.push_str(",\"slowest\":");
    let slowest = SLOWEST.lock().unwrap_or_else(|p| p.into_inner());
    let now = Instant::now();
    let recent: Vec<&Event> = slowest.iter().filter(|e| now.duration_since(e.at) < SLOWEST_WINDOW).collect();
    push_events(&mut out, &recent);
    drop(slowest);
    out.push('}');
    out
}

/// What `data.json?since=` answers.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_dashboard_data(since: i64) -> Str {
    Str::from_bytes(data(since).as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assets_are_there_and_small() {
        let total: usize = ASSETS.iter().map(|a| a.len()).sum();
        assert!(
            ASSETS[2].len() < 32 * 1024,
            "the script is {} bytes; the plan's budget is about 30 KB",
            ASSETS[2].len()
        );
        assert!(total < 60 * 1024, "the dashboard is {total} bytes");
        assert!(ASSETS[0].contains("{{base}}"));
        assert_eq!(lrt_dashboard_asset(3).len, 0);
    }

    #[test]
    fn quantiles_interpolate_within_a_bucket() {
        let mut buckets = [0u64; 17];
        assert_eq!(quantile(0.5, &buckets), None);
        // Ten requests between 0.5 ms and 1 ms: the median is halfway through that bucket.
        buckets[3] = 10;
        assert!((quantile(0.5, &buckets).unwrap() - 0.00075).abs() < 1e-12);
        assert!((quantile(1.0, &buckets).unwrap() - 0.001).abs() < 1e-12);
        // In the first bucket the lower bound is 0; above every bound, the largest bound.
        let mut first = [0u64; 17];
        first[0] = 4;
        assert!((quantile(0.25, &first).unwrap() - 0.000025).abs() < 1e-12);
        let mut last = [0u64; 17];
        last[16] = 1;
        assert_eq!(quantile(0.99, &last), Some(10.0));
    }

    #[test]
    fn strings_cannot_break_out_of_json_or_html() {
        let mut out = String::new();
        push_string(&mut out, "a\"b\\c\n</script><!--&\u{1}");
        assert_eq!(out, "\"a\\\"b\\\\c\\n\\u003c/script\\u003e\\u003c!--\\u0026\\u0001\"");
        let mut n = String::new();
        push_number(&mut n, Some(f64::NAN));
        push_number(&mut n, None);
        push_number(&mut n, Some(1.5));
        assert_eq!(n, "nullnull1.5");
    }

    #[test]
    fn events_keep_errors_and_the_slowest_without_queries() {
        EVENTS_ON.store(true, Ordering::SeqCst);
        note("GET", metrics::UNMATCHED, "/boom", 503, Some(1_000), "id-1".to_string());
        for i in 0..30u64 {
            note("GET", metrics::UNMATCHED, "/slow", 200, Some(1_000_000 + i), String::new());
        }
        let text = data(0);
        assert!(text.contains("\"path\":\"/boom\",\"status\":503"), "{text}");
        assert!(text.contains("\"id\":\"id-1\""));
        let slowest = SLOWEST.lock().unwrap();
        assert_eq!(slowest.len(), SLOWEST_KEPT);
        assert_eq!(slowest[0].nanos, Some(1_000_029));
        // The fastest kept is now the floor a request must pass.
        assert_eq!(SLOW_FLOOR.load(Ordering::Relaxed), slowest[SLOWEST_KEPT - 1].nanos.unwrap());
        assert!(!qualifies(200, Some(1_000)));
        assert!(qualifies(500, None));
    }
}
