//! Traces (docs/14, milestone 10): `tests/http/trace_server` runs with
//! `OTEL_EXPORTER_OTLP_ENDPOINT` pointing at a fake collector here, which records each OTLP/JSON
//! body POSTed to `/v1/traces` (and can be told to fail). The spans are read back with a small
//! JSON reader and checked: server spans with their attributes, a `traceparent` continued from
//! the client and sent on by `http.fetch`, the program's own spans, sampling ratios of 0 and 1,
//! dropping while the collector is down, the flush when the server stops, and `trace_id` in
//! log lines.

mod common;

use common::{connect, read_response, start_with_env};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const PROGRAM: &str = "trace_server";

// ---- a small JSON reader ------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Number(f64),
    Text(String),
    List(Vec<Json>),
    Object(BTreeMap<String, Json>),
}

impl Json {
    fn get(&self, key: &str) -> &Json {
        match self {
            Json::Object(map) => map.get(key).unwrap_or(&Json::Null),
            _ => &Json::Null,
        }
    }

    fn list(&self) -> &[Json] {
        match self {
            Json::List(items) => items,
            _ => &[],
        }
    }

    fn text(&self) -> &str {
        match self {
            Json::Text(text) => text,
            _ => "",
        }
    }
}

fn parse_json(text: &str) -> Json {
    fn skip(b: &[u8], i: &mut usize) {
        while *i < b.len() && b[*i].is_ascii_whitespace() {
            *i += 1;
        }
    }
    fn value(b: &[u8], i: &mut usize) -> Json {
        skip(b, i);
        match b[*i] {
            b'{' => {
                *i += 1;
                let mut map = BTreeMap::new();
                skip(b, i);
                if b[*i] == b'}' {
                    *i += 1;
                    return Json::Object(map);
                }
                loop {
                    skip(b, i);
                    let Json::Text(key) = value(b, i) else { panic!("a key that is not a string") };
                    skip(b, i);
                    assert_eq!(b[*i], b':');
                    *i += 1;
                    map.insert(key, value(b, i));
                    skip(b, i);
                    *i += 1;
                    if b[*i - 1] == b'}' {
                        return Json::Object(map);
                    }
                }
            }
            b'[' => {
                *i += 1;
                let mut items = Vec::new();
                skip(b, i);
                if b[*i] == b']' {
                    *i += 1;
                    return Json::List(items);
                }
                loop {
                    items.push(value(b, i));
                    skip(b, i);
                    *i += 1;
                    if b[*i - 1] == b']' {
                        return Json::List(items);
                    }
                }
            }
            b'"' => {
                *i += 1;
                let mut out = String::new();
                loop {
                    match b[*i] {
                        b'"' => {
                            *i += 1;
                            return Json::Text(out);
                        }
                        b'\\' => {
                            *i += 1;
                            match b[*i] {
                                b'n' => out.push('\n'),
                                b't' => out.push('\t'),
                                b'r' => out.push('\r'),
                                b'u' => {
                                    let code =
                                        u32::from_str_radix(std::str::from_utf8(&b[*i + 1..*i + 5]).unwrap(), 16)
                                            .unwrap();
                                    out.push(char::from_u32(code).unwrap_or('?'));
                                    *i += 4;
                                }
                                c => out.push(c as char),
                            }
                            *i += 1;
                        }
                        _ => {
                            let start = *i;
                            while b[*i] != b'"' && b[*i] != b'\\' {
                                *i += 1;
                            }
                            out.push_str(std::str::from_utf8(&b[start..*i]).unwrap());
                        }
                    }
                }
            }
            b't' => {
                *i += 4;
                Json::Bool(true)
            }
            b'f' => {
                *i += 5;
                Json::Bool(false)
            }
            b'n' => {
                *i += 4;
                Json::Null
            }
            _ => {
                let start = *i;
                while *i < b.len() && matches!(b[*i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
                    *i += 1;
                }
                Json::Number(std::str::from_utf8(&b[start..*i]).unwrap().parse().unwrap())
            }
        }
    }
    let mut i = 0;
    value(text.as_bytes(), &mut i)
}

// ---- the fake collector -------------------------------------------------------------------

struct Collector {
    port: u16,
    bodies: Arc<Mutex<Vec<String>>>,
    /// While set, answers 503 without keeping the body.
    failing: Arc<AtomicBool>,
}

fn collector() -> Collector {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let failing = Arc::new(AtomicBool::new(false));
    let (kept, fail) = (bodies.clone(), failing.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap_or(0);
            let target = line.split(' ').nth(1).unwrap_or("").to_string();
            let mut length = 0;
            let mut content_type = String::new();
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
                    break;
                }
                let (name, value) = header.split_once(':').unwrap_or(("", ""));
                match name.to_ascii_lowercase().as_str() {
                    "content-length" => length = value.trim().parse().unwrap_or(0),
                    "content-type" => content_type = value.trim().to_string(),
                    _ => {}
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let mut stream = stream;
            if fail.load(Ordering::SeqCst) {
                let _ = stream
                    .write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                continue;
            }
            assert_eq!(target, "/v1/traces");
            assert_eq!(content_type, "application/json");
            kept.lock().unwrap().push(String::from_utf8(body).unwrap());
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}");
        }
    });
    Collector { port, bodies, failing }
}

#[derive(Debug, Clone)]
struct Span {
    trace_id: String,
    span_id: String,
    parent: String,
    name: String,
    kind: f64,
    attributes: BTreeMap<String, String>,
    status: Json,
    service: String,
}

impl Collector {
    fn spans(&self) -> Vec<Span> {
        let mut spans = Vec::new();
        for body in self.bodies.lock().unwrap().iter() {
            let json = parse_json(body);
            for resource in json.get("resourceSpans").list() {
                let service = resource
                    .get("resource")
                    .get("attributes")
                    .list()
                    .iter()
                    .find(|a| a.get("key").text() == "service.name")
                    .map(|a| a.get("value").get("stringValue").text().to_string())
                    .unwrap_or_default();
                for scope in resource.get("scopeSpans").list() {
                    for span in scope.get("spans").list() {
                        let attributes = span
                            .get("attributes")
                            .list()
                            .iter()
                            .map(|a| {
                                let value = a.get("value");
                                let text = match (value.get("stringValue"), value.get("intValue")) {
                                    (Json::Text(s), _) => s.clone(),
                                    (_, Json::Text(n)) => n.clone(),
                                    other => panic!("an attribute of another kind: {other:?}"),
                                };
                                (a.get("key").text().to_string(), text)
                            })
                            .collect();
                        let Json::Number(kind) = span.get("kind") else { panic!("no kind in {span:?}") };
                        spans.push(Span {
                            trace_id: span.get("traceId").text().to_string(),
                            span_id: span.get("spanId").text().to_string(),
                            parent: span.get("parentSpanId").text().to_string(),
                            name: span.get("name").text().to_string(),
                            kind: *kind,
                            attributes,
                            status: span.get("status").clone(),
                            service: service.clone(),
                        });
                    }
                }
            }
        }
        spans
    }

    /// The spans, once at least `count` have arrived.
    fn wait_for(&self, count: usize) -> Vec<Span> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let spans = self.spans();
            if spans.len() >= count {
                return spans;
            }
            assert!(Instant::now() < deadline, "{} spans arrived, not {count}: {spans:#?}", spans.len());
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn get(port: u16, path: &str, headers: &str) -> common::Response {
    let (mut stream, mut reader) = connect(port);
    write!(stream, "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{headers}\r\n").unwrap();
    read_response(&mut reader, false)
}

fn endpoint(collector: &Collector) -> String {
    format!("http://127.0.0.1:{}", collector.port)
}

fn interval() -> String {
    lang_names::env_var("TRACE_INTERVAL_MS")
}

#[test]
fn spans_of_requests_fetches_and_blocks() {
    let collector = collector();
    let url = endpoint(&collector);
    let log = lang_names::env_var("LOG");
    let interval = interval();
    let mut server =
        start_with_env(PROGRAM, &[("OTEL_EXPORTER_OTLP_ENDPOINT", &url), (&log, "info"), (&interval, "50")]);
    let port = server.port;

    // A request carrying a trace continues it.
    let parent = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
    let continued = get(port, "/items/7?secret=1", &format!("traceparent: {parent}\r\ntracestate: vendor=abc\r\n"));
    assert_eq!(continued.body, "item 7");
    let spans = collector.wait_for(1);
    let server_span = &spans[0];
    assert_eq!(server_span.service, "trace-test");
    assert_eq!(server_span.name, "GET /items/{id}");
    assert_eq!(server_span.kind, 2.0);
    assert_eq!(server_span.trace_id, "4bf92f3577b34da6a3ce929d0e0e4736");
    assert_eq!(server_span.parent, "00f067aa0ba902b7");
    assert_eq!(server_span.span_id.len(), 16);
    let attributes: Vec<(&str, &str)> = server_span.attributes.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    assert_eq!(
        attributes,
        [
            ("client.address", "127.0.0.1"),
            ("http.request.method", "GET"),
            ("http.response.status_code", "200"),
            ("http.route", "/items/{id}"),
            ("url.path", "/items/7"),
        ]
    );
    assert_eq!(server_span.status, Json::Null);

    // A fetch inside a request is its child, and sends `traceparent` on.
    let chain = get(port, "/chain", "");
    assert_eq!(chain.status, 200, "{}", chain.body);
    let words: Vec<&str> = chain.body.split(' ').collect();
    let (trace_id, sent) = (words[1], words[3]);
    assert_eq!(trace_id.len(), 32);
    let spans = collector.wait_for(4);
    let of_trace: Vec<&Span> = spans.iter().filter(|s| s.trace_id == trace_id).collect();
    assert_eq!(of_trace.len(), 3, "{of_trace:#?}");
    let outer = of_trace.iter().find(|s| s.name == "GET /chain").unwrap();
    let client = of_trace.iter().find(|s| s.kind == 3.0).unwrap();
    let inner = of_trace.iter().find(|s| s.name == "GET /hello").unwrap();
    assert_eq!(outer.parent, "", "a new trace's first span has no parent");
    assert_eq!(client.parent, outer.span_id);
    assert_eq!(inner.parent, client.span_id);
    assert_eq!(sent, format!("00-{trace_id}-{}-01", client.span_id));
    assert_eq!(client.name, "GET");
    assert_eq!(client.attributes["http.request.method"], "GET");
    assert_eq!(client.attributes["server.address"], "127.0.0.1");
    assert_eq!(client.attributes["server.port"], port.to_string());
    assert_eq!(client.attributes["url.full"], format!("http://127.0.0.1:{port}/hello"), "the query is left out");
    assert_eq!(client.attributes["http.response.status_code"], "200");

    // The program's own spans nest, and keep their attributes and failures.
    assert_eq!(get(port, "/work", "").body, "worked");
    let spans = collector.wait_for(7);
    let resize = spans.iter().find(|s| s.name == "resize image").unwrap();
    let encode = spans.iter().find(|s| s.name == "encode").unwrap();
    let request = spans.iter().find(|s| s.name == "GET /work").unwrap();
    assert_eq!((resize.parent.as_str(), resize.kind), (request.span_id.as_str(), 1.0));
    assert_eq!(encode.parent, resize.span_id);
    assert_eq!(resize.attributes["width"], "800");
    assert_eq!(resize.attributes["photo.id"], "7");
    assert_eq!(encode.status.get("code"), &Json::Number(2.0));
    assert_eq!(encode.status.get("message").text(), "out of colours");

    // A panic is a failed span with a 500.
    assert_eq!(get(port, "/boom", "").status, 500);
    let spans = collector.wait_for(8);
    let boom = spans.iter().find(|s| s.name == "GET /boom").unwrap();
    assert_eq!(boom.attributes["http.response.status_code"], "500");
    assert_eq!(boom.status.get("code"), &Json::Number(2.0));

    // A request no route took is named by its method alone.
    assert_eq!(get(port, "/nowhere", "").status, 404);
    let spans = collector.wait_for(9);
    assert!(
        spans.iter().any(|s| s.name == "GET" && s.kind == 2.0 && !s.attributes.contains_key("http.route")),
        "{spans:#?}"
    );

    server.stop(PROGRAM);
    let log = server.stderr.lock().unwrap().clone();
    let hello = log.lines().find(|l| l.contains("saying hello")).unwrap_or_else(|| panic!("{log}"));
    assert!(hello.contains(&format!("trace_id={trace_id}")), "{hello}");
    assert!(hello.contains(&format!("span_id={}", inner.span_id)), "{hello}");
    let access = log.lines().find(|l| l.contains("GET /items/7 200")).unwrap();
    assert!(access.contains("trace_id=4bf92f3577b34da6a3ce929d0e0e4736"), "{access}");
}

#[test]
fn a_ratio_of_0_records_nothing_but_passes_the_trace_on() {
    let collector = collector();
    let url = endpoint(&collector);
    let interval = interval();
    let mut server = start_with_env(
        PROGRAM,
        &[("OTEL_EXPORTER_OTLP_ENDPOINT", &url), ("OTEL_TRACES_SAMPLER_ARG", "0"), (&interval, "50")],
    );
    let port = server.port;
    for _ in 0..20 {
        assert_eq!(get(port, "/items/1", "").status, 200);
    }
    let chain = get(port, "/chain", "");
    let words: Vec<&str> = chain.body.split(' ').collect();
    assert_eq!(words[1].len(), 32, "{}", chain.body);
    assert!(words[3].starts_with(&format!("00-{}-", words[1])) && words[3].ends_with("-00"), "{}", chain.body);
    // A caller that sampled the trace wins over the ratio.
    get(port, "/items/2", "traceparent: 00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01\r\n");
    let spans = collector.wait_for(1);
    std::thread::sleep(Duration::from_millis(300));
    let spans_after = collector.spans();
    assert_eq!(spans_after.len(), 1, "{spans_after:#?}");
    assert_eq!(spans[0].trace_id, "0af7651916cd43dd8448eb211c80319c");
    server.stop(PROGRAM);
}

#[test]
fn spans_wait_for_the_collector_and_are_flushed_when_the_server_stops() {
    let collector = collector();
    let url = endpoint(&collector);
    // The default interval of 5 s: nothing is sent while the test runs, only at the end.
    let mut server = start_with_env(PROGRAM, &[("OTEL_EXPORTER_OTLP_ENDPOINT", &url)]);
    let port = server.port;
    for i in 0..5 {
        assert_eq!(get(port, &format!("/items/{i}"), "").status, 200);
    }
    assert_eq!(collector.spans().len(), 0);
    Command::new("kill").args(["-TERM", &server.child.id().to_string()]).status().unwrap();
    let exit = server.child.wait().unwrap();
    assert!(exit.success(), "{exit}");
    assert_eq!(collector.spans().len(), 5, "the spans were not sent when the server stopped");
}

#[test]
fn spans_are_dropped_and_counted_while_the_collector_is_down() {
    let collector = collector();
    collector.failing.store(true, Ordering::SeqCst);
    let url = endpoint(&collector);
    let interval = interval();
    let log = lang_names::env_var("LOG");
    let mut server =
        start_with_env(PROGRAM, &[("OTEL_EXPORTER_OTLP_ENDPOINT", &url), (&interval, "50"), (&log, "info")]);
    let port = server.port;
    for _ in 0..50 {
        assert_eq!(get(port, "/items/1", "").status, 200);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let dropped = loop {
        let metrics = get(port, "/metrics", "").body;
        let dropped: f64 = metrics
            .lines()
            .find_map(|l| l.strip_prefix("trace_spans_dropped_total "))
            .unwrap_or_else(|| panic!("{metrics}"))
            .parse()
            .unwrap();
        if dropped >= 50.0 || Instant::now() > deadline {
            break dropped;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(dropped >= 50.0, "{dropped}");
    // Back up: later spans arrive again.
    collector.failing.store(false, Ordering::SeqCst);
    get(port, "/items/9", "");
    let spans = collector.wait_for(1);
    assert!(spans.iter().any(|s| s.attributes.get("url.path").is_some_and(|p| p == "/items/9")), "{spans:#?}");
    server.stop(PROGRAM);
    let log = server.stderr.lock().unwrap().clone();
    assert_eq!(log.matches("could not send").count(), 1, "one warning for the outage: {log}");
    assert!(log.contains("the collector answered 503"), "{log}");
    assert!(log.contains("exporting traces again"), "{log}");
}

#[test]
fn queued_spans_are_capped() {
    // A collector that never answers: spans pile up to the cap, and the rest are dropped.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let mut server = start_with_env(PROGRAM, &[("OTEL_EXPORTER_OTLP_ENDPOINT", &url)]);
    let port = server.port;
    let (mut stream, mut reader) = connect(port);
    for _ in 0..3000 {
        write!(stream, "GET /items/1 HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
    }
    for _ in 0..3000 {
        assert_eq!(read_response(&mut reader, false).status, 200);
    }
    let metrics = get(port, "/metrics", "").body;
    let dropped: f64 =
        metrics.lines().find_map(|l| l.strip_prefix("trace_spans_dropped_total ")).unwrap().parse().unwrap();
    // At most 2048 wait, and one batch of 512 may be with the collector that does not answer,
    // if the exporter took it before the queue filled.
    assert!((3000.0 - 2048.0 - 512.0..=3000.0 - 2048.0).contains(&dropped), "{metrics}");
    drop(listener);
    server.stop(PROGRAM);
}
