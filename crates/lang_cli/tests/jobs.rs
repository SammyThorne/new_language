//! Background jobs in a running server (docs/14, milestone 19): a schedule runs while the server
//! serves, a full or closed queue is refused, and SIGTERM lets the running job finish, drops the
//! queued ones with a count in the log, and starts nothing new.

mod common;

use common::{build_runtime, connect, read_response, start};
use std::io::Write;
use std::time::{Duration, Instant};

fn request(port: u16, method: &str, path: &str) -> common::Response {
    let (mut stream, mut reader) = connect(port);
    stream
        .write_all(
            format!("{method} {path} HTTP/1.1\r\nHost: test\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .unwrap();
    read_response(&mut reader, false)
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn sigterm_finishes_the_running_job_and_drops_the_queue() {
    build_runtime();
    let mut server = start("jobs_server");
    let port = server.port;
    // The schedule runs by itself.
    wait_for("the schedule to run", || request(port, "GET", "/runs").body.parse::<i64>().unwrap() >= 2);
    assert_eq!(request(port, "POST", "/enqueue?n=1").body, "queued");
    std::thread::sleep(Duration::from_millis(250));
    assert_eq!(request(port, "POST", "/enqueue?n=2").body, "queued");
    assert_eq!(request(port, "POST", "/enqueue?n=3").body, "queued");
    let started = Instant::now();
    server.stop("jobs_server");
    // The server waited for item 1 (0.6 s of work, started at least 0.25 s before the signal).
    assert!(started.elapsed() < Duration::from_secs(5));
    let log = server.stderr.lock().unwrap().clone();
    assert!(log.contains("handled 1"), "the running job did not finish:\n{log}");
    assert!(!log.contains("handled 2") && !log.contains("handled 3"), "queued jobs ran after SIGTERM:\n{log}");
    assert!(log.contains("queue work: 2 queued item(s) dropped at shutdown"), "no count of dropped items:\n{log}");
}
