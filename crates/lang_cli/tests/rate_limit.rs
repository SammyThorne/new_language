//! Rate limits (docs/14, milestone 14) over the wire: `tests/http/rate_limit_server.vi`, with
//! 1000 requests at once against a burst of 50, the headers, and 400,000 distinct keys with the
//! server's memory measured. The burst, refusal and refill on a fixed clock are in
//! `tests/run/http_rate_limit.vi`; the limiter's own tests are in `lang_rt`.

mod common;

use common::{connect, read_response, start};
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const PROGRAM: &str = "rate_limit_server";

/// The server's resident memory, in KiB.
fn resident(pid: u32) -> u64 {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    let line = status.lines().find(|l| l.starts_with("VmRSS:")).unwrap();
    line.split_whitespace().nth(1).unwrap().parse().unwrap()
}

#[test]
fn no_more_than_the_burst_out_of_1000_at_once() {
    let mut server = start(PROGRAM);
    let port = server.port;
    // 100 connections, each sending 10 requests in one write, all released together.
    let barrier = Arc::new(std::sync::Barrier::new(100));
    let (allowed, refused) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let threads: Vec<_> = (0..100)
        .map(|_| {
            let (barrier, allowed, refused) = (barrier.clone(), allowed.clone(), refused.clone());
            std::thread::spawn(move || {
                let (mut stream, mut reader) = connect(port);
                barrier.wait();
                stream.write_all("GET /burst HTTP/1.1\r\nHost: test\r\n\r\n".repeat(10).as_bytes()).unwrap();
                for _ in 0..10 {
                    let response = read_response(&mut reader, false);
                    match response.status {
                        200 => allowed.fetch_add(1, Ordering::Relaxed),
                        429 => refused.fetch_add(1, Ordering::Relaxed),
                        other => panic!("status {other}"),
                    };
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!((allowed.load(Ordering::Relaxed), refused.load(Ordering::Relaxed)), (50, 950));
    server.stop(PROGRAM);
}

#[test]
fn responses_carry_the_headers() {
    let mut server = start(PROGRAM);
    let (mut stream, mut reader) = connect(server.port);
    let mut statuses = Vec::new();
    for _ in 0..4 {
        stream.write_all(b"POST /login HTTP/1.1\r\nHost: test\r\nContent-Length: 0\r\n\r\n").unwrap();
        let response = read_response(&mut reader, false);
        statuses.push(response.status);
        assert_eq!(response.header("ratelimit-policy"), Some("route;q=3;w=60"));
        if response.status == 429 {
            assert_eq!(response.header("retry-after"), Some("20"));
            assert_eq!(response.header("ratelimit"), Some("route;a=0;w=20"));
            assert_eq!(response.body, "{\"error\":\"too many requests\"}");
        }
    }
    assert_eq!(statuses, [200, 200, 200, 429]);
    stream.write_all(b"GET /keyed HTTP/1.1\r\nHost: test\r\nX-Key: k\r\n\r\n").unwrap();
    let response = read_response(&mut reader, false);
    assert_eq!(response.header("ratelimit"), Some("default;a=4;w=1"));
    server.stop(PROGRAM);
}

#[test]
fn hundreds_of_thousands_of_keys_keep_memory_bounded() {
    let mut server = start(PROGRAM);
    let port = server.port;
    let pid = server.child.id();
    // 8 connections, each sending its share in batches of 500 pipelined requests.
    let send = |from: usize, to: usize| {
        let threads: Vec<_> = (0..8)
            .map(|t| {
                std::thread::spawn(move || {
                    let (mut stream, mut reader) = connect(port);
                    let mine: Vec<usize> = (from..to).filter(|k| k % 8 == t).collect();
                    for batch in mine.chunks(500) {
                        let mut requests = String::new();
                        for key in batch {
                            requests
                                .push_str(&format!("GET /keyed HTTP/1.1\r\nHost: test\r\nX-Key: client-{key}\r\n\r\n"));
                        }
                        stream.write_all(requests.as_bytes()).unwrap();
                        for _ in batch {
                            assert_eq!(read_response(&mut reader, false).status, 200);
                        }
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
    };
    let before = resident(pid);
    send(0, 200_000);
    let full = resident(pid);
    send(200_000, 400_000);
    let after = resident(pid);
    eprintln!("resident memory: {before} KiB, {full} KiB after 200,000 keys, {after} KiB after 200,000 more");
    // Past the cap of 100,000, new keys replace old ones instead of adding to them: the second
    // 200,000 cost a small part of what the first did (about 15 MiB per 100,000 without a cap).
    let first = full.saturating_sub(before);
    let second = after.saturating_sub(full);
    assert!(second * 4 < first.max(8192), "{first} KiB for the first 200,000 keys, {second} KiB for the next");
    // The newest keys are limited, so still known.
    let (mut stream, mut reader) = connect(port);
    stream.write_all(b"GET /keyed HTTP/1.1\r\nHost: test\r\nX-Key: client-399999\r\n\r\n").unwrap();
    assert_eq!(read_response(&mut reader, false).header("ratelimit"), Some("default;a=3;w=2"));
    server.stop(PROGRAM);
}
