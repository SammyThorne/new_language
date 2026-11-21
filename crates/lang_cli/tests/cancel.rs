//! Timeouts and cancellation (docs/14, milestone 13): `tests/http/cancel_server.vi` with route,
//! group, annotation and server timeouts, a spawned task that inherits the deadline, a loop
//! that checks, clients that hang up in the middle of a request, pipelined requests, a
//! `connect_timeout` against an address that never answers, and, with a database URL in
//! `<PREFIX>TEST_DATABASE_URL`, a `postgres` query stopped at the deadline.

mod common;

use common::{Response, connect, read_response, start, start_with_env};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

const PROGRAM: &str = "cancel_server";

fn get(port: u16, path: &str) -> Response {
    let (mut stream, mut reader) = connect(port);
    stream.write_all(format!("GET {path} HTTP/1.1\r\nHost: test\r\n\r\n").as_bytes()).unwrap();
    read_response(&mut reader, false)
}

/// The value of a sample in `/metrics`.
fn metric(port: u16, sample: &str) -> String {
    let body = get(port, "/metrics").body;
    let line = body.lines().find(|l| l.starts_with(sample)).unwrap_or_else(|| panic!("no {sample} in\n{body}"));
    line.rsplit(' ').next().unwrap().to_string()
}

#[test]
fn handlers_past_their_timeout_get_a_503_and_stop() {
    let mut server = start(PROGRAM);
    let port = server.port;
    for path in ["/slow", "/api/slow", "/annotated", "/spawner", "/spin"] {
        let started = Instant::now();
        let response = get(port, path);
        let took = started.elapsed();
        assert_eq!(response.status, 503, "{path}: {}", response.body);
        assert_eq!(response.header("retry-after"), Some("1"), "{path}");
        assert_eq!(response.body, "the request took longer than its timeout\n");
        assert!(took < Duration::from_secs(2), "{path} took {took:?}");
    }
    // The work after each handler's wait never ran; the spawned task's sleep ended early too.
    assert_eq!(get(port, "/late").body, "0");
    assert_eq!(get(port, "/events").body, "nap woke early=True");
    // Other routes have the server's timeout.
    assert_eq!(get(port, "/deadline").body, "deadline in 30s");
    assert_eq!(metric(port, "http_request_errors_total{kind=\"timeout\"}"), "5");
    let log = server.stderr.lock().unwrap().clone();
    assert!(log.contains("GET /slow 503"), "{log}");
    server.stop(PROGRAM);
}

#[test]
fn a_client_that_hangs_up_cancels_its_handler() {
    let server = start(PROGRAM);
    let port = server.port;
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.write_all(b"GET /hang HTTP/1.1\r\nHost: test\r\n\r\n").unwrap();
    std::thread::sleep(Duration::from_millis(300));
    drop(stream);
    // The handler's channel receive ended with nothing, well before anything else would end it.
    assert_eq!(get(port, "/events").body, "hang got=None cancelled=True quick=True");
    assert_eq!(metric(port, "http_request_errors_total{kind=\"client_closed\"}"), "1");
}

#[test]
fn pipelined_requests_are_not_hang_ups() {
    let server = start(PROGRAM);
    let (mut stream, mut reader) = connect(server.port);
    // Both requests arrive together; the second is data, not a hang-up, while the first runs.
    stream.write_all(b"GET /pause HTTP/1.1\r\nHost: test\r\n\r\nGET /pause HTTP/1.1\r\nHost: test\r\n\r\n").unwrap();
    for _ in 0..2 {
        let response = read_response(&mut reader, false);
        assert_eq!((response.status, response.body.as_str()), (200, "paused cancelled=False"));
    }
    // Closing only the sending side after a request is still a hang-up, as in Go: the handler is
    // cancelled, and its response is still written.
    stream.write_all(b"GET /pause HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n").unwrap();
    stream.shutdown(std::net::Shutdown::Write).unwrap();
    let mut rest = String::new();
    reader.read_to_string(&mut rest).unwrap();
    assert!(rest.ends_with("paused cancelled=True"), "{rest}");
}

#[test]
fn connect_timeout_against_an_address_that_never_answers() {
    use std::os::fd::AsRawFd;
    unsafe extern "C" {
        fn listen(fd: i32, backlog: i32) -> i32;
    }
    let server = start(PROGRAM);
    // A listener that never accepts, with a backlog of none: once a connection or two wait in it,
    // the kernel drops new connection attempts, so they neither connect nor fail.
    let blackhole = TcpListener::bind("127.0.0.1:0").unwrap();
    // SAFETY: changing the backlog of a listening socket.
    assert_eq!(unsafe { listen(blackhole.as_raw_fd(), 0) }, 0);
    let address = blackhole.local_addr().unwrap();
    let mut waiting = Vec::new();
    for _ in 0..4 {
        if let Ok(stream) = TcpStream::connect_timeout(&address, Duration::from_millis(200)) {
            waiting.push(stream);
        }
    }
    let started = Instant::now();
    let response = get(server.port, &format!("/connect?to={address}"));
    assert_eq!(
        response.body,
        format!("http://{address}/: timed out connecting to {address} (connect_timeout=0.3s) quick=True")
    );
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn a_postgres_query_stops_at_the_deadline() {
    let Ok(url) = std::env::var(lang_names::env_var("TEST_DATABASE_URL")) else {
        eprintln!("skipped: set {} to run the PostgreSQL test", lang_names::env_var("TEST_DATABASE_URL"));
        return;
    };
    let server = start_with_env(PROGRAM, &[("DATABASE_URL", &url)]);
    let port = server.port;
    assert_eq!(get(port, "/query").status, 503);
    assert_eq!(get(port, "/events").body, "query 08006 cancelled: the request's deadline passed quick=True");
    // The CancelRequest stopped the query on the server, and the pool still works.
    let mut running = String::new();
    for _ in 0..20 {
        running = get(port, "/running").body;
        if running == "running 0" {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(running, "running 0");
}
