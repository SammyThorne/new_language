//! Streams and Server-Sent Events (docs/14, milestone 15) over the wire:
//! `tests/http/stream_server.vi`, with chunked bodies read here and by the language's own
//! client, a 1 GiB stream with the server's memory measured, events, heartbeats, clients that
//! hang up, streams past their route's timeout, a stream that fails, HTTP/1.0, keep-alive after
//! a stream, and the dashboard's event stream. Test requests collecting a stream are in
//! `tests/run/http_stream.vi`.

mod common;

use common::{connect, read_response, start, start_with_env};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

const PROGRAM: &str = "stream_server";

/// The status line and headers of a response, lower-cased names.
fn read_head(reader: &mut BufReader<TcpStream>) -> (u16, Vec<(String, String)>) {
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let status = line.split(' ').nth(1).unwrap().parse().unwrap();
    let mut headers = Vec::new();
    loop {
        line.clear();
        reader.read_line(&mut line).unwrap();
        let line = line.trim_end();
        if line.is_empty() {
            return (status, headers);
        }
        let (name, value) = line.split_once(':').unwrap();
        headers.push((name.to_ascii_lowercase(), value.trim().to_string()));
    }
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
}

/// Read a chunked body, checking its framing, and hand each chunk to `each`. Returns the number
/// of chunks, or an error if the body was cut short.
fn read_chunks(reader: &mut BufReader<TcpStream>, mut each: impl FnMut(&[u8])) -> Result<usize, String> {
    let mut chunks = 0;
    let mut line = String::new();
    let mut buffer = Vec::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            return Err(format!("the body ended after {chunks} chunks without its last chunk"));
        }
        let size = usize::from_str_radix(line.trim_end(), 16).map_err(|_| format!("bad chunk size {line:?}"))?;
        if size == 0 {
            line.clear();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, "\r\n", "the last chunk ends the body");
            return Ok(chunks);
        }
        buffer.resize(size, 0);
        reader.read_exact(&mut buffer).map_err(|e| e.to_string())?;
        each(&buffer);
        let mut end = [0u8; 2];
        reader.read_exact(&mut end).map_err(|e| e.to_string())?;
        assert_eq!(&end, b"\r\n", "each chunk ends with CRLF");
        chunks += 1;
    }
}

/// A GET that accepts gzip: streams must not be compressed all the same.
fn get(stream: &mut TcpStream, path: &str) {
    stream.write_all(format!("GET {path} HTTP/1.1\r\nHost: test\r\nAccept-Encoding: gzip\r\n\r\n").as_bytes()).unwrap();
}

/// A GET for an ordinary (not streamed) response, read with `read_response`.
fn get_plain(stream: &mut TcpStream, path: &str) {
    stream.write_all(format!("GET {path} HTTP/1.1\r\nHost: test\r\n\r\n").as_bytes()).unwrap();
}

/// The server's resident memory, in KiB.
fn resident(pid: u32) -> u64 {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    let line = status.lines().find(|l| l.starts_with("VmRSS:")).unwrap();
    line.split_whitespace().nth(1).unwrap().parse().unwrap()
}

#[test]
fn chunked_bodies_are_framed_and_read_by_the_client() {
    let mut server = start(PROGRAM);
    let port = server.port;
    let (mut stream, mut reader) = connect(port);
    get(&mut stream, "/rows");
    let (status, headers) = read_head(&mut reader);
    assert_eq!(status, 200);
    assert_eq!(header(&headers, "transfer-encoding"), Some("chunked"));
    assert_eq!(header(&headers, "content-length"), None);
    // Streams are not compressed, even when the client asks.
    assert_eq!(header(&headers, "content-encoding"), None);
    let mut body = Vec::new();
    read_chunks(&mut reader, |chunk| body.extend_from_slice(chunk)).unwrap();
    let text = String::from_utf8(body).unwrap();
    assert_eq!(text.lines().count(), 1000);
    assert_eq!(text.lines().last(), Some("row 999"));
    // The connection is still good for the next request.
    get_plain(&mut stream, &format!("/fetch?port={port}"));
    let response = read_response(&mut reader, false);
    assert_eq!(response.body, "200 1000 lines, last row 999");
    server.stop(PROGRAM);
}

#[test]
fn a_gigabyte_streams_in_flat_memory() {
    let mut server = start(PROGRAM);
    let pid = server.child.id();
    let (mut stream, mut reader) = connect(server.port);
    get(&mut stream, "/big?mib=1024");
    let (status, _) = read_head(&mut reader);
    assert_eq!(status, 200);
    let (mut total, mut peak, mut seen) = (0u64, 0u64, 0u64);
    let started = Instant::now();
    read_chunks(&mut reader, |chunk| {
        total += chunk.len() as u64;
        // Look at the server's memory every 64 MiB.
        if total / (64 << 20) != seen {
            seen = total / (64 << 20);
            peak = peak.max(resident(pid));
        }
    })
    .unwrap();
    eprintln!("1 GiB in {:?}; the server's memory peaked at {peak} KiB", started.elapsed());
    assert_eq!(total, 1 << 30);
    assert!(peak < 64 * 1024, "the server held {peak} KiB while streaming");
    server.stop(PROGRAM);
}

#[test]
fn events_stop_when_the_client_hangs_up() {
    let mut server = start(PROGRAM);
    let port = server.port;
    let (mut stream, mut reader) = connect(port);
    get(&mut stream, "/ticks");
    let (status, headers) = read_head(&mut reader);
    assert_eq!(status, 200);
    assert_eq!(header(&headers, "content-type"), Some("text/event-stream"));
    assert_eq!(header(&headers, "cache-control"), Some("no-store"));
    assert_eq!(header(&headers, "x-accel-buffering"), Some("no"));
    // Read three events (each a chunk), then hang up.
    let mut events = Vec::new();
    let mut line = String::new();
    let mut event = String::new();
    while events.len() < 3 {
        line.clear();
        reader.read_line(&mut line).unwrap();
        match line.as_str() {
            "\n" => events.push(std::mem::take(&mut event)),
            l if l.starts_with("id: ") || l.starts_with("data: ") => event.push_str(l),
            _ => {}
        }
    }
    drop(reader);
    drop(stream);
    assert_eq!(events, ["id: 0\ndata: tick 0\n", "id: 1\ndata: tick 1\n", "id: 2\ndata: tick 2\n"]);
    let (mut other, mut other_reader) = connect(port);
    get_plain(&mut other, "/log");
    assert_eq!(read_response(&mut other_reader, false).body, "ticker stopped after a failed send, closed=True");
    server.stop(PROGRAM);
}

#[test]
fn heartbeats_keep_a_quiet_stream_open_and_notice_a_hang_up() {
    let mut server = start(PROGRAM);
    let port = server.port;
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    get(&mut stream, "/quiet");
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let _ = read_head(&mut reader);
    // Two heartbeats in half a second, each its own chunk.
    let mut line = String::new();
    let mut pings = 0;
    let started = Instant::now();
    while pings < 2 && started.elapsed() < Duration::from_secs(3) {
        line.clear();
        reader.read_line(&mut line).unwrap();
        if line == ": ping\n" {
            pings += 1;
        }
    }
    assert_eq!(pings, 2);
    drop(reader);
    drop(stream);
    let (mut other, mut other_reader) = connect(port);
    get_plain(&mut other, "/log");
    assert_eq!(read_response(&mut other_reader, false).body, "quiet ended, closed=True");
    server.stop(PROGRAM);
}

#[test]
fn a_stream_outlives_its_routes_timeout() {
    let mut server = start(PROGRAM);
    let (mut stream, mut reader) = connect(server.port);
    get(&mut stream, "/slow");
    let (status, _) = read_head(&mut reader);
    assert_eq!(status, 200);
    let mut body = Vec::new();
    // Ten rows 0.1 s apart, each sent as it is written, past the route's timeout of 0.3 s.
    let chunks = read_chunks(&mut reader, |chunk| body.extend_from_slice(chunk)).unwrap();
    assert_eq!(chunks, 10);
    assert!(String::from_utf8(body).unwrap().ends_with("slow 9\n"));
    server.stop(PROGRAM);
}

#[test]
fn a_failed_stream_is_cut_short() {
    let mut server = start(PROGRAM);
    let (mut stream, mut reader) = connect(server.port);
    get(&mut stream, "/broken");
    let (status, _) = read_head(&mut reader);
    assert_eq!(status, 200);
    let mut body = Vec::new();
    let result = read_chunks(&mut reader, |chunk| body.extend_from_slice(chunk));
    assert_eq!(body, b"partial\n");
    assert!(result.unwrap_err().contains("without its last chunk"));
    server.stop(PROGRAM);
    let log = server.stderr.lock().unwrap().clone();
    assert!(log.contains("GET /broken: the stream failed: the export broke"), "{log}");
}

#[test]
fn http_1_0_streams_end_with_the_connection() {
    let mut server = start(PROGRAM);
    let mut stream = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
    stream.write_all(b"GET /rows HTTP/1.0\r\n\r\n").unwrap();
    let mut reader = BufReader::new(stream);
    let (status, headers) = read_head(&mut reader);
    assert_eq!(status, 200);
    assert_eq!(header(&headers, "transfer-encoding"), None);
    assert_eq!(header(&headers, "connection"), Some("close"));
    let mut body = String::new();
    reader.read_to_string(&mut body).unwrap();
    assert_eq!(body.lines().count(), 1000);
    server.stop(PROGRAM);
}

#[test]
fn the_dashboard_sends_samples_as_events() {
    let mut server = start_with_env(PROGRAM, &[(&lang_names::env_var("METRICS_INTERVAL_MS"), "100")]);
    // Samples 1 to 3 at least, so that resuming after sample 2 has something to send.
    std::thread::sleep(Duration::from_millis(600));
    let (mut stream, mut reader) = connect(server.port);
    stream.write_all(b"GET /metrics/dashboard/events HTTP/1.1\r\nHost: test\r\nLast-Event-ID: 2\r\n\r\n").unwrap();
    let (status, headers) = read_head(&mut reader);
    assert_eq!(status, 200);
    assert_eq!(header(&headers, "content-type"), Some("text/event-stream"));
    assert_eq!(header(&headers, "x-frame-options"), Some("DENY"));
    let mut ids = Vec::new();
    let mut text = String::new();
    let mut line = String::new();
    // Chunk sizes and data lines; the ids of the first events.
    while ids.len() < 3 {
        line.clear();
        reader.read_line(&mut line).unwrap();
        text.push_str(&line);
        if let Some(id) = line.strip_prefix("id: ") {
            ids.push(id.trim().parse::<u64>().unwrap());
        }
    }
    // Resumed after sample 2, and the samples come in order.
    assert!(ids[0] >= 3 && ids[1] > ids[0] && ids[2] > ids[1], "{ids:?}");
    assert!(text.contains("data: {\"version\":"), "{text}");
    assert!(!text.contains("{\"seq\":1,") && !text.contains("{\"seq\":2,"), "{text}");
    drop(stream);
    server.stop(PROGRAM);
}
