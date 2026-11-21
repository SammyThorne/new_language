//! Metrics, health and readiness (docs/14, milestone 8): `tests/http/metrics_server` is loaded
//! from 64 connections and its `/metrics` checked to the request, its text format checked by a
//! small parser, its Basic authentication and startup refusal tried, and its readiness checks
//! and drain on SIGTERM watched.

mod common;

use common::{compiler, connect, read_response, repo_root, start_with_env};
use lang_names::{CLI_NAME, SOURCE_EXTENSION, env_var};
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::process::Command;
use std::time::{Duration, Instant};

const PROGRAM: &str = "metrics_server";
const CREDENTIALS: [(&str, &str); 2] = [("METRICS_USER", "ops"), ("METRICS_PASSWORD", "hunter2:with a colon")];

fn base64(text: &str) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in text.as_bytes().chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            out.push(if i <= chunk.len() { ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

fn request(port: u16, method: &str, path: &str, headers: &str) -> common::Response {
    let (mut stream, mut reader) = connect(port);
    write!(stream, "{method} {path} HTTP/1.1\r\nHost: test\r\nContent-Length: 0\r\n{headers}\r\n").unwrap();
    read_response(&mut reader, method == "HEAD")
}

fn scrape(port: u16) -> Exposition {
    let auth = format!("Authorization: Basic {}\r\n", base64("ops:hunter2:with a colon"));
    let response = request(port, "GET", "/metrics", &auth);
    assert_eq!(response.status, 200, "{}", response.body);
    assert_eq!(response.header("content-type"), Some("text/plain; version=0.0.4; charset=utf-8"));
    assert_eq!(response.header("cache-control"), Some("no-store"));
    parse(&response.body).unwrap_or_else(|e| panic!("{e}\n{}", response.body))
}

// ---- a checker of the Prometheus text format (version 0.0.4) --------------------------------

/// The samples, keyed `name{label="value",...}` with the labels sorted and their values
/// unescaped, and each family's type.
struct Exposition {
    samples: BTreeMap<String, f64>,
    types: HashMap<String, String>,
}

impl Exposition {
    fn get(&self, key: &str) -> f64 {
        *self.samples.get(key).unwrap_or_else(|| panic!("no {key} in {:#?}", self.samples.keys().collect::<Vec<_>>()))
    }

    /// The sum of the samples of `name` whose labels include all of `labels`.
    fn sum(&self, name: &str, labels: &[(&str, &str)]) -> f64 {
        self.samples
            .iter()
            .filter(|(key, _)| {
                key.split('{').next() == Some(name)
                    && labels.iter().all(|(l, v)| key.contains(&format!("{l}={}", quote(v))))
            })
            .map(|(_, v)| v)
            .sum()
    }
}

fn quote(value: &str) -> String {
    format!("{value:?}")
}

fn key(name: &str, labels: &BTreeMap<String, String>) -> String {
    if labels.is_empty() {
        return name.to_string();
    }
    let inner: Vec<String> = labels.iter().map(|(l, v)| format!("{l}={}", quote(v))).collect();
    format!("{name}{{{}}}", inner.join(","))
}

fn valid_name(name: &str, colons: bool) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || (colons && c == ':'))
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || (colons && c == ':'))
}

/// Parse `name{label="value",...}`: the name, the labels, and the rest of the line.
fn parse_series(line: &str) -> Result<(String, BTreeMap<String, String>, &str), String> {
    let end = line.find(['{', ' ']).ok_or(format!("no value in {line:?}"))?;
    let name = &line[..end];
    if !valid_name(name, true) {
        return Err(format!("bad metric name {name:?}"));
    }
    let mut labels = BTreeMap::new();
    let mut rest = &line[end..];
    if let Some(inner) = rest.strip_prefix('{') {
        let mut chars = inner.char_indices().peekable();
        loop {
            let start = chars.peek().map(|(i, _)| *i).ok_or("unclosed labels")?;
            if inner[start..].starts_with('}') {
                rest = &inner[start + 1..];
                break;
            }
            let eq = inner[start..].find('=').ok_or("a label without `=`")? + start;
            let label = &inner[start..eq];
            if !valid_name(label, false) {
                return Err(format!("bad label name {label:?}"));
            }
            while chars.peek().is_some_and(|(i, _)| *i <= eq) {
                chars.next();
            }
            if chars.next().map(|(_, c)| c) != Some('"') {
                return Err(format!("label {label} has no opening quote"));
            }
            let mut value = String::new();
            loop {
                match chars.next().ok_or("unclosed label value")?.1 {
                    '"' => break,
                    '\\' => match chars.next().ok_or("dangling escape")?.1 {
                        '\\' => value.push('\\'),
                        '"' => value.push('"'),
                        'n' => value.push('\n'),
                        other => return Err(format!("unknown escape \\{other}")),
                    },
                    '\n' => return Err("a raw newline in a label value".into()),
                    c => value.push(c),
                }
            }
            if labels.insert(label.to_string(), value).is_some() {
                return Err(format!("label {label} twice in {line:?}"));
            }
            if chars.peek().is_some_and(|(_, c)| *c == ',') {
                chars.next();
            }
        }
    }
    Ok((name.to_string(), labels, rest))
}

fn parse_value(text: &str) -> Result<f64, String> {
    match text {
        "+Inf" => Ok(f64::INFINITY),
        "-Inf" => Ok(f64::NEG_INFINITY),
        "NaN" => Ok(f64::NAN),
        _ => text.parse().map_err(|_| format!("bad value {text:?}")),
    }
}

fn parse(text: &str) -> Result<Exposition, String> {
    let mut samples = BTreeMap::new();
    let mut types: HashMap<String, String> = HashMap::new();
    let mut helps: Vec<String> = Vec::new();
    let mut finished: Vec<String> = Vec::new();
    let mut current: Option<String> = None;
    // Per histogram series (labels without `le`): its buckets in order.
    let mut buckets: BTreeMap<String, Vec<(f64, f64)>> = BTreeMap::new();
    if !text.ends_with('\n') {
        return Err("the text does not end with a newline".into());
    }
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# HELP ") {
            let (name, help) = rest.split_once(' ').unwrap_or((rest, ""));
            if helps.contains(&name.to_string()) {
                return Err(format!("two HELP lines for {name}"));
            }
            if help.contains("\\") && help.replace("\\\\", "").replace("\\n", "").contains('\\') {
                return Err(format!("a bad escape in the HELP of {name}"));
            }
            helps.push(name.to_string());
            continue;
        }
        if let Some(rest) = line.strip_prefix("# TYPE ") {
            let (name, kind) = rest.split_once(' ').ok_or("a TYPE line without a type")?;
            if !["counter", "gauge", "histogram", "summary", "untyped"].contains(&kind) {
                return Err(format!("unknown type {kind}"));
            }
            if types.insert(name.to_string(), kind.to_string()).is_some() || finished.contains(&name.to_string()) {
                return Err(format!("two TYPE lines for {name}"));
            }
            if let Some(previous) = current.replace(name.to_string()) {
                finished.push(previous);
            }
            continue;
        }
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let (name, labels, rest) = parse_series(line)?;
        let value = parse_value(rest.trim_start_matches(' ').split(' ').next().unwrap_or(""))?;
        let family = current.clone().ok_or(format!("{name} has no TYPE line before it"))?;
        let kind = &types[&family];
        let suffix = name.strip_prefix(&family).ok_or(format!("{name} is outside its family {family}"))?;
        let allowed: &[&str] = match kind.as_str() {
            "histogram" => &["_bucket", "_sum", "_count"],
            "summary" => &["", "_sum", "_count"],
            _ => &[""],
        };
        if !allowed.contains(&suffix) {
            return Err(format!("{name} does not belong to the {kind} {family}"));
        }
        if suffix == "_bucket" {
            let le = labels.get("le").ok_or(format!("{line:?} has no le"))?;
            let mut rest_labels = labels.clone();
            rest_labels.remove("le");
            buckets.entry(key(&family, &rest_labels)).or_default().push((parse_value(le)?, value));
        } else if kind == "histogram" && labels.contains_key("le") {
            return Err(format!("{line:?} has le outside a bucket"));
        }
        if samples.insert(key(&name, &labels), value).is_some() {
            return Err(format!("{line:?} repeats a series"));
        }
    }
    for (series, list) in &buckets {
        if list.last().map(|(le, _)| *le) != Some(f64::INFINITY) {
            return Err(format!("{series} does not end with le=\"+Inf\""));
        }
        if !list.windows(2).all(|w| w[0].0 < w[1].0 && w[0].1 <= w[1].1) {
            return Err(format!("{series}'s buckets are not increasing and cumulative"));
        }
        let (family, labels) =
            series.split_once('{').map_or((series.as_str(), String::new()), |(f, l)| (f, format!("{{{l}")));
        let count = samples.get(&format!("{family}_count{labels}")).ok_or(format!("{series} has no _count"))?;
        if *count != list.last().unwrap().1 || !samples.contains_key(&format!("{family}_sum{labels}")) {
            return Err(format!("{series}: _count differs from the +Inf bucket, or there is no _sum"));
        }
    }
    Ok(Exposition { samples, types })
}

#[test]
fn the_checker_rejects_what_it_should() {
    for bad in [
        "x 1\n",
        "# TYPE x counter\nx{a=\"b} 1\n",
        "# TYPE x counter\nx{a=\"\\t\"} 1\n",
        "# TYPE x counter\nx 1\nx 2\n",
        "# TYPE h histogram\nh_bucket{le=\"1\"} 1\nh_sum 1\nh_count 1\n",
        "# TYPE h histogram\nh_bucket{le=\"1\"} 2\nh_bucket{le=\"+Inf\"} 1\nh_sum 1\nh_count 1\n",
        "# TYPE x counter\n# TYPE x counter\n",
        "# TYPE 1x counter\n1x 1\n",
    ] {
        assert!(parse(bad).is_err(), "accepted {bad:?}");
    }
    let good = parse("# HELP x a \\\\ b\\nc\n# TYPE x counter\nx{a=\"q\\\"\\\\\\n\"} 1\n").unwrap();
    assert_eq!(good.get("x{a=\"q\\\"\\\\\\n\"}"), 1.0);
}

// ---- the server -----------------------------------------------------------------------------

#[test]
fn exact_totals_after_100000_requests_on_64_connections() {
    let mut server = start_with_env(PROGRAM, &CREDENTIALS);
    let port = server.port;
    const TOTAL: usize = 100_000;
    const CONNECTIONS: usize = 64;
    // Per kind of request: how many, and their body bytes.
    let threads: Vec<_> = (0..CONNECTIONS)
        .map(|t| {
            std::thread::spawn(move || {
                let (mut stream, mut reader) = connect(port);
                let mut counts = [0u64; 5];
                let mut bytes = [0u64; 5];
                let mine: Vec<usize> = (t..TOTAL).step_by(CONNECTIONS).collect();
                // Pipelined, 32 at a time, to spend the time in the server rather than waiting.
                for batch in mine.chunks(32) {
                    let mut sent = String::new();
                    let mut expected = Vec::new();
                    for &i in batch {
                        let kind = i % 5;
                        let (method, path, status) = match kind {
                            0 => ("GET", format!("/items/{i}"), 200),
                            1 => ("POST", "/items".to_string(), 201),
                            2 => ("GET", format!("/missing/{i}"), 404),
                            3 => ("DELETE", "/items/1".to_string(), 405),
                            _ => ("HEAD", format!("/items/{i}"), 200),
                        };
                        sent.push_str(&format!("{method} {path} HTTP/1.1\r\nHost: test\r\nContent-Length: 0\r\n\r\n"));
                        expected.push((kind, method, path, status));
                    }
                    stream.write_all(sent.as_bytes()).unwrap();
                    for (kind, method, path, status) in expected {
                        let response = read_response(&mut reader, method == "HEAD");
                        assert_eq!(response.status, status, "{method} {path}");
                        counts[kind] += 1;
                        bytes[kind] += response.body.len() as u64;
                    }
                }
                (counts, bytes)
            })
        })
        .collect();
    let mut counts = [0u64; 5];
    let mut bytes = [0u64; 5];
    for thread in threads {
        let (c, b) = thread.join().unwrap();
        for k in 0..5 {
            counts[k] += c[k];
            bytes[k] += b[k];
        }
    }
    assert_eq!(counts.iter().sum::<u64>(), TOTAL as u64);
    let m = scrape(port);
    let total = |method: &str, route: &str, status: &str| {
        m.get(&format!("http_requests_total{{method=\"{method}\",route=\"{route}\",status=\"{status}\"}}"))
    };
    assert_eq!(total("GET", "/items/{id}", "200"), counts[0] as f64);
    assert_eq!(total("POST", "/items", "201"), counts[1] as f64);
    assert_eq!(total("GET", "unmatched", "404"), counts[2] as f64);
    assert_eq!(total("DELETE", "unmatched", "405"), counts[3] as f64);
    assert_eq!(total("HEAD", "/items/{id}", "200"), counts[4] as f64);
    assert_eq!(m.sum("http_requests_total", &[]), TOTAL as f64, "the metrics' own requests are not counted");
    // The histogram is per route, under the route's own method; HEAD bodies are not sent.
    let items = "{method=\"GET\",route=\"/items/{id}\"}";
    assert_eq!(m.get(&format!("http_request_duration_seconds_count{items}")), (counts[0] + counts[4]) as f64);
    assert_eq!(m.get(&format!("http_response_size_bytes_sum{items}")), bytes[0] as f64);
    assert_eq!(m.get("http_request_duration_seconds_count{method=\"POST\",route=\"/items\"}"), counts[1] as f64);
    assert_eq!(m.get("http_response_size_bytes_sum{method=\"POST\",route=\"/items\"}"), bytes[1] as f64);
    assert_eq!(m.get("http_request_duration_seconds_count{route=\"unmatched\"}"), (counts[2] + counts[3]) as f64);
    assert_eq!(m.get("http_response_size_bytes_sum{route=\"unmatched\"}"), (bytes[2] + bytes[3]) as f64);
    assert!(m.get(&format!("http_request_duration_seconds_sum{items}")) > 0.0);
    assert_eq!(m.get("http_connections_total"), (CONNECTIONS + 1) as f64);
    assert_eq!(m.get("http_requests_in_flight"), 1.0, "the scrape itself");
    assert_eq!(m.sum("http_request_errors_total", &[]), 0.0);
    for (name, kind) in [
        ("http_requests_total", "counter"),
        ("http_request_duration_seconds", "histogram"),
        ("http_response_size_bytes", "summary"),
        ("process_resident_memory_bytes", "gauge"),
        ("runtime_heap_bytes", "gauge"),
        ("runtime_gc_collections_total", "counter"),
        ("build_info", "gauge"),
    ] {
        assert_eq!(m.types.get(name).map(String::as_str), Some(kind), "{name}");
    }
    assert!(m.get("process_resident_memory_bytes") > 1e6);
    assert!(m.get("runtime_worker_threads") >= 1.0);
    assert!(m.get("process_start_time_seconds") > 1.7e9);
    server.stop(PROGRAM);
}

#[test]
fn errors_labels_authentication_and_the_access_log() {
    let mut server = start_with_env(PROGRAM, &[CREDENTIALS[0], CREDENTIALS[1], (&env_var("LOG"), "info")]);
    let port = server.port;
    for headers in [
        String::new(),
        format!("Authorization: Basic {}\r\n", base64("ops:hunter2")),
        format!("Authorization: Basic {}\r\n", base64("root:hunter2:with a colon")),
        "Authorization: Bearer abc\r\n".to_string(),
    ] {
        let response = request(port, "GET", "/metrics", &headers);
        assert_eq!(response.status, 401, "{headers:?}");
        assert_eq!(response.header("www-authenticate"), Some("Basic realm=\"metrics\", charset=\"UTF-8\""));
        assert!(!response.body.contains("http_requests_total"));
    }
    assert_eq!(request(port, "GET", "/boom", "").status, 500);
    assert_eq!(request(port, "GET", "/items/7", "").status, 200);
    assert_eq!(request(port, "GET", "/healthz", "").status, 200);
    // A label value with every character the format escapes.
    assert_eq!(request(port, "GET", "/tag?value=a%22b%5Cc%0Ad", "").status, 200);
    let (mut stream, mut reader) = connect(port);
    stream.write_all(b"NOT A REQUEST\r\n\r\n").unwrap();
    assert_eq!(read_response(&mut reader, false).status, 400);
    let m = scrape(port);
    assert_eq!(m.get("http_request_errors_total{kind=\"panic\"}"), 1.0);
    assert_eq!(m.get("http_request_errors_total{kind=\"parse\"}"), 1.0);
    assert_eq!(m.get("http_requests_total{method=\"GET\",route=\"/boom\",status=\"500\"}"), 1.0);
    assert_eq!(m.get(&format!("tags_total{{tag={}}}", quote("a\"b\\c\nd"))), 1.0);
    // The metrics and health routes are in neither the metrics nor the access log.
    assert!(!m.samples.keys().any(|k| k.contains("/metrics") || k.contains("/healthz")), "{:?}", m.samples.keys());
    assert_eq!(m.sum("http_requests_total", &[]), 3.0);
    server.stop(PROGRAM);
    let log = server.stderr.lock().unwrap().clone();
    let access = log.lines().find(|l| l.contains("GET /items/7 200")).unwrap_or_else(|| panic!("{log}"));
    assert!(access.contains(" route=/items/{id} "), "{access}");
    assert!(!log.contains("/healthz"), "{log}");
    assert!(log.contains("GET /metrics 401"), "{log}");
}

#[test]
fn metrics_without_credentials_refuse_to_start() {
    let executable = std::env::temp_dir().join(format!("{CLI_NAME}-metrics-refuse-{}", std::process::id()));
    let output = Command::new(compiler())
        .args(["build", &format!("tests/http/{PROGRAM}.{SOURCE_EXTENSION}"), "-o"])
        .arg(&executable)
        .current_dir(repo_root())
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let output = Command::new(&executable)
        .env_remove("METRICS_USER")
        .env_remove("METRICS_PASSWORD")
        .current_dir(repo_root())
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&executable);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("server.metrics needs a username and password: set METRICS_USER and METRICS_PASSWORD in the environment or in .env (read by config.load or config.load_env), or pass public=True to serve the metrics without them."),
        "{stderr}"
    );
    assert!(!stderr.contains("listening on"), "{stderr}");
}

#[test]
fn readiness_checks_and_the_drain_on_sigterm() {
    let mut server = start_with_env(PROGRAM, &CREDENTIALS);
    let port = server.port;
    let live = request(port, "GET", "/healthz", "");
    assert_eq!((live.status, live.body.as_str()), (200, "ok\n"));
    let ready = request(port, "GET", "/readyz", "");
    assert_eq!((ready.status, ready.body.as_str()), (200, "{\"cache\":\"ok\",\"database\":\"ok\"}"));
    assert_eq!(ready.header("content-type"), Some("application/json"));
    request(port, "POST", "/database/1", "");
    let ready = request(port, "GET", "/readyz", "");
    assert_eq!((ready.status, ready.body.as_str()), (503, "{\"cache\":\"ok\",\"database\":\"connection refused\"}"));
    request(port, "POST", "/database/2", "");
    let started = Instant::now();
    let ready = request(port, "GET", "/readyz", "");
    let waited = started.elapsed();
    assert_eq!(ready.status, 503);
    assert_eq!(ready.body, "{\"cache\":\"ok\",\"database\":\"did not answer within 2 s\"}");
    assert!(waited >= Duration::from_millis(1900) && waited < Duration::from_millis(2900), "{waited:?}");
    request(port, "POST", "/database/0", "");
    // SIGTERM: readiness fails at once, and everything else is served for the drain delay.
    let stopping = Instant::now();
    Command::new("kill").args(["-TERM", &server.child.id().to_string()]).status().unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let ready = request(port, "GET", "/readyz", "");
        if ready.status == 503 {
            assert_eq!(ready.body, "{\"server\":\"draining\"}");
            break;
        }
        assert!(Instant::now() < deadline, "readiness did not fail after SIGTERM");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(stopping.elapsed() < Duration::from_millis(900));
    assert_eq!(request(port, "GET", "/healthz", "").status, 200);
    assert_eq!(request(port, "GET", "/items/3", "").body, "item 3");
    let exit = server.child.wait().unwrap();
    assert!(exit.success(), "{exit}");
    let took = stopping.elapsed();
    assert!(took >= Duration::from_secs(1) && took < Duration::from_secs(4), "{took:?}");
    assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_err(), "still accepting after the drain");
}
