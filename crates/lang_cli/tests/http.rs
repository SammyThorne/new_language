//! The milestone 8 exit test: the todo service in `tests/http/`, written once with route
//! annotations and once in the call style, must answer the same script of HTTP requests the same
//! way. Each program is compiled, started, sent the script over raw TCP, then stopped with SIGTERM,
//! which must shut it down cleanly.

mod common;

use common::{Response, build_runtime, connect, read_response, start};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;

fn request(
    stream: &mut TcpStream,
    reader: &mut BufReader<TcpStream>,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> Response {
    let mut text = format!("{method} {path} HTTP/1.1\r\nHost: test\r\nUser-Agent: tester\r\n");
    if let Some(body) = body {
        text.push_str(&format!("Content-Type: application/json\r\nContent-Length: {}\r\n", body.len()));
    }
    text.push_str("\r\n");
    text.push_str(body.unwrap_or(""));
    stream.write_all(text.as_bytes()).unwrap();
    read_response(reader, method == "HEAD")
}

/// One step of the script: method, path, body, expected status, expected body or `None` to skip.
type Step = (&'static str, &'static str, Option<&'static str>, u16, Option<&'static str>);

const SCRIPT: &[Step] = &[
    ("GET", "/health", None, 200, Some("ok")),
    ("GET", "/api/todos", None, 200, Some("[]")),
    (
        "POST",
        "/api/todos",
        Some(r#"{"title": "write docs"}"#),
        201,
        Some(r#"{"id":1,"title":"write docs","done":false}"#),
    ),
    ("POST", "/api/todos", Some(r#"{"title": "ship"}"#), 201, Some(r#"{"id":2,"title":"ship","done":false}"#)),
    ("GET", "/api/todos/1", None, 200, Some(r#"{"id":1,"title":"write docs","done":false}"#)),
    ("PATCH", "/api/todos/2", Some(r#"{"done": true}"#), 200, Some(r#"{"id":2,"title":"ship","done":true}"#)),
    ("GET", "/api/todos?done=true", None, 200, Some(r#"[{"id":2,"title":"ship","done":true}]"#)),
    ("GET", "/api/todos?limit=1", None, 200, Some(r#"[{"id":1,"title":"write docs","done":false}]"#)),
    ("GET", "/api/todos?limit=x", None, 400, Some(r#"{"error":"query parameter `limit` must be an integer"}"#)),
    ("GET", "/api/todos/9", None, 404, Some(r#"{"error":"todo not found"}"#)),
    ("GET", "/api/todos/abc", None, 400, Some(r#"{"error":"path parameter `id` must be an integer"}"#)),
    (
        "POST",
        "/api/todos",
        Some(r#"{"titl": 1}"#),
        400,
        Some(r#"{"error":"invalid JSON body: missing key \"title\""}"#),
    ),
    ("POST", "/api/todos", Some(r#"{"title": "  "}"#), 422, Some(r#"{"error":"the title is empty"}"#)),
    ("PUT", "/api/todos/1", Some("{}"), 405, Some(r#"{"error":"method not allowed"}"#)),
    ("GET", "/nowhere", None, 404, Some(r#"{"error":"not found"}"#)),
    ("DELETE", "/api/todos/1", None, 204, Some("")),
    ("GET", "/api/todos", None, 200, Some(r#"[{"id":2,"title":"ship","done":true}]"#)),
    ("GET", "/echo/a/b/c", None, 200, Some("GET a/b/c from tester")),
    ("HEAD", "/health", None, 200, Some("")),
];

fn run_script(program: &str) {
    let mut server = start(program);
    let (mut stream, mut reader) = connect(server.port);
    for (method, path, body, status, expected) in SCRIPT {
        let response = request(&mut stream, &mut reader, method, path, *body);
        assert_eq!(response.status, *status, "{program}: {method} {path} gave {}: {}", response.status, response.body);
        if let Some(expected) = expected {
            assert_eq!(response.body, *expected, "{program}: {method} {path}");
        }
        assert_eq!(response.header("connection"), Some("keep-alive"), "{program}: {method} {path}");
        assert!(response.header("date").is_some());
    }
    let allow = request(&mut stream, &mut reader, "PUT", "/api/todos/2", Some("{}"));
    assert_eq!(allow.header("allow"), Some("GET, PATCH, DELETE"), "{program}: Allow header");

    // A chunked body, sent after `Expect: 100-continue` is answered.
    stream
        .write_all(
            b"POST /api/todos HTTP/1.1\r\nHost: test\r\nTransfer-Encoding: chunked\r\nExpect: 100-continue\r\n\r\n",
        )
        .unwrap();
    let mut interim = String::new();
    reader.read_line(&mut interim).unwrap();
    assert!(interim.starts_with("HTTP/1.1 100"), "{program}: expected 100 Continue, got {interim:?}");
    let mut blank = String::new();
    reader.read_line(&mut blank).unwrap();
    stream.write_all(b"8\r\n{\"title\"\r\n9\r\n: \"chunk\"\r\n1\r\n}\r\n0\r\n\r\n").unwrap();
    let chunked = read_response(&mut reader, false);
    assert_eq!((chunked.status, chunked.body.as_str()), (201, r#"{"id":3,"title":"chunk","done":false}"#), "{program}");

    // Pipelined requests on one write.
    stream.write_all(b"GET /health HTTP/1.1\r\nHost: t\r\n\r\nGET /api/todos/3 HTTP/1.1\r\nHost: t\r\n\r\n").unwrap();
    assert_eq!(read_response(&mut reader, false).body, "ok");
    assert_eq!(read_response(&mut reader, false).status, 200);

    // `Connection: close` is honoured, and malformed requests get a 400.
    stream.write_all(b"GET /health HTTP/1.1\r\nConnection: close\r\n\r\n").unwrap();
    let closing = read_response(&mut reader, false);
    assert_eq!(closing.header("connection"), Some("close"));
    let (mut bad, mut bad_reader) = connect(server.port);
    bad.write_all(b"NONSENSE\r\n\r\n").unwrap();
    assert_eq!(read_response(&mut bad_reader, false).status, 400);

    // A handler that panics gets a 500 and the connection is closed; the server carries on,
    // with a panic in some task, not the whole process.
    let (mut panicky, mut panicky_reader) = connect(server.port);
    let failed = request(&mut panicky, &mut panicky_reader, "GET", "/boom", None);
    assert_eq!((failed.status, failed.header("connection")), (500, Some("close")), "{program}: /boom");

    // Concurrent clients.
    let port = server.port;
    let clients: Vec<_> = (0..8)
        .map(|_| {
            std::thread::spawn(move || {
                let (mut s, mut r) = connect(port);
                for _ in 0..25 {
                    assert_eq!(request(&mut s, &mut r, "GET", "/health", None).body, "ok");
                }
            })
        })
        .collect();
    for client in clients {
        client.join().unwrap();
    }

    server.stop(program);
}

#[test]
fn todo_service_with_annotations() {
    build_runtime();
    run_script("todo_annotations");
}

#[test]
fn todo_service_in_call_style() {
    build_runtime();
    run_script("todo_calls");
}
