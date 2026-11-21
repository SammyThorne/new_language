//! Request size limits and lazy bodies (docs/14, milestone 11): `tests/http/limits_server.vi`
//! with the server's, a group's, a route's and an annotation's `max_body`, bodies at and one
//! past each limit by `Content-Length` and chunked, `100-continue` only when the body is read,
//! draining a body the handler did not read, and the limits on headers and the target.

mod common;

use common::{Response, connect, read_response, start};
use std::io::{BufRead, Read, Write};
use std::time::Duration;

const PROGRAM: &str = "limits_server";

/// A POST with a body of `size` bytes, on a connection of its own.
fn post(port: u16, path: &str, size: usize) -> Response {
    let (mut stream, mut reader) = connect(port);
    let head = format!("POST {path} HTTP/1.1\r\nHost: test\r\nContent-Length: {size}\r\n\r\n");
    stream.write_all(head.as_bytes()).unwrap();
    // The server may answer 413 and close before taking it all.
    let _ = stream.write_all(&vec![b'a'; size]);
    read_response(&mut reader, false)
}

/// A chunked body of `size` bytes, in chunks of at most 4000.
fn chunked(size: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut left = size;
    while left > 0 {
        let n = left.min(4000);
        out.extend_from_slice(format!("{n:x}\r\n").as_bytes());
        out.extend(std::iter::repeat_n(b'a', n));
        out.extend_from_slice(b"\r\n");
        left -= n;
    }
    out.extend_from_slice(b"0\r\n\r\n");
    out
}

fn post_chunked(port: u16, path: &str, size: usize) -> Response {
    let (mut stream, mut reader) = connect(port);
    let head = format!("POST {path} HTTP/1.1\r\nHost: test\r\nTransfer-Encoding: chunked\r\n\r\n");
    stream.write_all(head.as_bytes()).unwrap();
    let _ = stream.write_all(&chunked(size));
    read_response(&mut reader, false)
}

/// Whether the server closes the connection: the next read sees the end.
fn closed(reader: &mut std::io::BufReader<std::net::TcpStream>) -> bool {
    let mut rest = Vec::new();
    reader.read_to_end(&mut rest).is_ok() && rest.is_empty()
}

#[test]
fn bodies_past_each_limit_get_413() {
    let server = start(PROGRAM);
    let port = server.port;
    // (path, limit): the server's, a group's, a route of the group's own, a route's past 64 KiB,
    // and an annotation's.
    for (path, limit) in [("/echo", 1000), ("/g/echo", 50), ("/g/wide", 500), ("/big", 3_000_000), ("/note", 10)] {
        let at = post(port, path, limit);
        assert_eq!(at.status, 200, "{path} at {limit}: {}", at.body);
        if path == "/note" {
            assert_eq!(at.body, format!("note {}", "a".repeat(limit)));
        } else {
            assert_eq!(at.body, format!("read {limit}"), "{path}");
        }
        let past = post(port, path, limit + 1);
        assert_eq!(past.status, 413, "{path} at {}: {}", limit + 1, past.body);
        // A body past the server's 1000 bytes is left unread, so the connection closes; a
        // smaller one was read with the head, and the connection is kept.
        let close = if limit + 1 > 1000 { Some("close") } else { Some("keep-alive") };
        assert_eq!(past.header("connection"), close, "{path}");
        let at = post_chunked(port, path, limit);
        assert_eq!(at.status, 200, "{path} chunked at {limit}: {}", at.body);
        let past = post_chunked(port, path, limit + 1);
        assert_eq!(past.status, 413, "{path} chunked at {}: {}", limit + 1, past.body);
    }
    // `req.body()` has no error to return: the client gets the 413 all the same.
    assert_eq!(post(port, "/raw", 1000).body, "bytes 1000");
    assert_eq!(post(port, "/raw", 1001).status, 413);
    // So does a handler that ignores the error.
    assert_eq!(post(port, "/swallow", 1000).body, "got 1000");
    assert_eq!(post(port, "/swallow", 1001).status, 413);
}

#[test]
fn continue_is_sent_only_when_the_body_is_read() {
    let server = start(PROGRAM);
    let port = server.port;

    // A handler that reads the body: `100 Continue`, then the answer.
    let (mut stream, mut reader) = connect(port);
    stream
        .write_all(b"POST /echo HTTP/1.1\r\nHost: test\r\nContent-Length: 5\r\nExpect: 100-continue\r\n\r\n")
        .unwrap();
    let mut interim = String::new();
    reader.read_line(&mut interim).unwrap();
    assert!(interim.starts_with("HTTP/1.1 100"), "{interim:?}");
    let mut blank = String::new();
    reader.read_line(&mut blank).unwrap();
    assert_eq!(blank, "\r\n");
    stream.write_all(b"hello").unwrap();
    let answer = read_response(&mut reader, false);
    assert_eq!((answer.status, answer.body.as_str()), (200, "read 5"));

    // One that does not: the answer at once, and the connection closes, since the body may or
    // may not follow.
    let (mut stream, mut reader) = connect(port);
    stream
        .write_all(b"POST /ignore HTTP/1.1\r\nHost: test\r\nContent-Length: 5000\r\nExpect: 100-continue\r\n\r\n")
        .unwrap();
    let answer = read_response(&mut reader, false);
    assert_eq!((answer.status, answer.body.as_str()), (200, "ignored"));
    assert_eq!(answer.header("connection"), Some("close"));
    assert!(closed(&mut reader));

    // A body past the limit is refused before it is sent.
    let (mut stream, mut reader) = connect(port);
    stream
        .write_all(b"POST /echo HTTP/1.1\r\nHost: test\r\nContent-Length: 5000\r\nExpect: 100-continue\r\n\r\n")
        .unwrap();
    let answer = read_response(&mut reader, false);
    assert_eq!(answer.status, 413, "{}", answer.body);
}

#[test]
fn a_pipelined_request_after_a_drained_body() {
    let server = start(PROGRAM);
    let port = server.port;
    let (mut stream, mut reader) = connect(port);
    // 100,000 bytes, past the eager 64 KiB and past the server's limit, which the handler does
    // not read: drained, and the next request on the connection is served.
    let mut bytes = b"POST /ignore HTTP/1.1\r\nHost: test\r\nContent-Length: 100000\r\n\r\n".to_vec();
    bytes.extend(std::iter::repeat_n(b'a', 100_000));
    bytes.extend_from_slice(b"POST /echo HTTP/1.1\r\nHost: test\r\nContent-Length: 5\r\n\r\nhello");
    // A chunked body that is not read, then another request.
    bytes.extend_from_slice(b"POST /ignore HTTP/1.1\r\nHost: test\r\nTransfer-Encoding: chunked\r\n\r\n");
    bytes.extend(chunked(200_000));
    bytes.extend_from_slice(b"POST /echo HTTP/1.1\r\nHost: test\r\nContent-Length: 2\r\n\r\nhi");
    stream.write_all(&bytes).unwrap();
    for expected in ["ignored", "read 5", "ignored", "read 2"] {
        let answer = read_response(&mut reader, false);
        assert_eq!((answer.status, answer.body.as_str()), (200, expected));
        assert_ne!(answer.header("connection"), Some("close"));
    }
}

#[test]
fn a_connection_closes_after_an_undrained_large_body() {
    let server = start(PROGRAM);
    let port = server.port;
    let (mut stream, mut reader) = connect(port);
    // 3 MiB announced, more than the 1 MiB worth draining: the answer says the connection
    // closes, and it does, without waiting for the rest.
    stream.write_all(b"POST /ignore HTTP/1.1\r\nHost: test\r\nContent-Length: 3145728\r\n\r\n").unwrap();
    stream.write_all(&vec![b'a'; 10_000]).unwrap();
    let answer = read_response(&mut reader, false);
    assert_eq!((answer.status, answer.body.as_str()), (200, "ignored"));
    assert_eq!(answer.header("connection"), Some("close"));
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    assert!(closed(&mut reader));
}

#[test]
fn too_many_headers_and_too_long_a_target() {
    let server = start(PROGRAM);
    let port = server.port;
    let send = |text: String| {
        let (mut stream, mut reader) = connect(port);
        stream.write_all(text.as_bytes()).unwrap();
        read_response(&mut reader, false)
    };
    let headers = |n: usize| (0..n).map(|i| format!("X-H{i}: v\r\n")).collect::<String>();
    // Host and Content-Length are two of the 20.
    let at = send(format!("POST /echo HTTP/1.1\r\nHost: t\r\nContent-Length: 0\r\n{}\r\n", headers(18)));
    assert_eq!(at.status, 200, "{}", at.body);
    let past = send(format!("POST /echo HTTP/1.1\r\nHost: t\r\nContent-Length: 0\r\n{}\r\n", headers(19)));
    assert_eq!(past.status, 431);
    // The target, query string included, may be 200 bytes.
    let path = |n: usize| format!("/echo?q={}", "x".repeat(n - 8));
    let at = send(format!("POST {} HTTP/1.1\r\nHost: t\r\nContent-Length: 0\r\n\r\n", path(200)));
    assert_eq!(at.status, 200, "{}", at.body);
    let past = send(format!("POST {} HTTP/1.1\r\nHost: t\r\nContent-Length: 0\r\n\r\n", path(201)));
    assert_eq!(past.status, 414);
}
