//! Forms and uploads (docs/14, milestone 12): `tests/http/upload_server.vi` with urlencoded and
//! multipart forms decoded into `http.Form[T]`, `http.Upload` parameters (one, optional and a
//! list), files large enough to wait on disk and their removal when the request ends, `save`,
//! the form limits, and the documented request bodies.

mod common;

use common::{Response, connect, read_response, start_with_env};
use std::io::Write;
use std::path::{Path, PathBuf};

const PROGRAM: &str = "upload_server";

/// A directory of its own for the server's large uploads.
fn upload_dir(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("forms-test-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn send(port: u16, path: &str, content_type: &str, body: &[u8]) -> Response {
    let (mut stream, mut reader) = connect(port);
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: test\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).unwrap();
    let _ = stream.write_all(body);
    read_response(&mut reader, false)
}

/// A part of a multipart body: (name, file name, content type, data).
type Part<'a> = (&'a str, Option<&'a str>, Option<&'a str>, &'a [u8]);

fn multipart(parts: &[Part]) -> Vec<u8> {
    let mut body = Vec::new();
    for (name, filename, content_type, data) in parts {
        body.extend_from_slice(b"--BOUNDARY\r\nContent-Disposition: form-data; name=\"");
        body.extend_from_slice(name.as_bytes());
        body.push(b'"');
        if let Some(filename) = filename {
            body.extend_from_slice(format!("; filename=\"{filename}\"").as_bytes());
        }
        body.extend_from_slice(b"\r\n");
        if let Some(content_type) = content_type {
            body.extend_from_slice(format!("Content-Type: {content_type}\r\n").as_bytes());
        }
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(data);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(b"--BOUNDARY--\r\n");
    body
}

const MULTIPART: &str = "multipart/form-data; boundary=BOUNDARY";

fn waiting(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect()
}

#[test]
fn form_fields_are_decoded_into_the_type() {
    let dir = upload_dir("fields");
    let server = start_with_env(PROGRAM, &[("UPLOAD_DIR", dir.to_str().unwrap())]);
    let port = server.port;
    let urlencoded = "application/x-www-form-urlencoded";
    let ok = send(port, "/signup", urlencoded, b"name=Ada+Lovelace&age=36&tags=a&tags=b%26c&agree=on");
    assert_eq!(ok.status, 200, "{}", ok.body);
    assert_eq!(ok.body, r#"{"name":"Ada Lovelace","age":36,"tags":["a","b&c"],"agree":true}"#);
    let body = multipart(&[("name", None, None, b"Bo"), ("age", None, None, b" 7 ")]);
    let ok = send(port, "/signup", MULTIPART, &body);
    assert_eq!(ok.body, r#"{"name":"Bo","age":7,"tags":[],"agree":false}"#);
    let bad = send(port, "/signup", urlencoded, b"age=old");
    assert_eq!(bad.status, 400);
    assert_eq!(
        bad.body,
        r#"{"error":"invalid form: field `age` must be an integer, got \"old\"; field `name` is required"}"#
    );
    let json = send(port, "/signup", "application/json", br#"{"name":"x","age":1}"#);
    assert_eq!(json.status, 415, "{}", json.body);
    let malformed = send(port, "/signup", urlencoded, b"name=%zz");
    assert_eq!(malformed.status, 400);
    assert_eq!(malformed.body, r#"{"error":"the form has a malformed `%` escape"}"#);
}

#[test]
fn uploads_bind_and_large_ones_wait_on_disk_until_the_request_ends() {
    let dir = upload_dir("uploads");
    let mut server = start_with_env(PROGRAM, &[("UPLOAD_DIR", dir.to_str().unwrap())]);
    let port = server.port;
    // A small file stays in memory: nothing waits on disk.
    let body =
        multipart(&[("caption", None, None, b"hi"), ("avatar", Some("me.png"), Some("image/png"), b"\x89PNG\r\n\x00")]);
    let small = send(port, "/avatar", MULTIPART, &body);
    assert_eq!(small.status, 200, "{}", small.body);
    assert_eq!(small.body, "me.png image/png 7 7 'hi' 0");
    // Over 1 MiB it waits in the upload directory while the handler runs, and is gone after.
    let big: Vec<u8> = (0..1_500_000u32).map(|i| (i % 251) as u8).collect();
    let body = multipart(&[("avatar", Some("C:\\photos\\big.bin"), None, &big)]);
    let large = send(port, "/avatar", MULTIPART, &body);
    assert_eq!(large.body, "big.bin application/octet-stream 1500000 1500000 '' 1");
    assert_eq!(waiting(&dir), Vec::<String>::new());
    // The file part is required.
    let missing = send(port, "/avatar", MULTIPART, &multipart(&[("caption", None, None, b"x")]));
    assert_eq!(missing.status, 400);
    assert_eq!(missing.body, r#"{"error":"missing file `avatar`"}"#);
    // `save` moves the waiting file; nothing else is left.
    let body = multipart(&[("file", Some("../../kept.bin"), None, &big)]);
    let kept = send(port, "/keep", MULTIPART, &body);
    assert_eq!(kept.body, "kept 1500000");
    assert_eq!(waiting(&dir), ["kept-kept.bin"]);
    assert_eq!(std::fs::read(dir.join("kept-kept.bin")).unwrap(), big);
    // A list takes every file of the name, and an optional one may be left out.
    let body = multipart(&[("docs", Some("a.txt"), None, b"aa"), ("docs", Some("b.txt"), None, b"bbb")]);
    assert_eq!(send(port, "/many", MULTIPART, &body).body, "a.txt:2 b.txt:3 no note");
    let body = multipart(&[("note", Some("n.md"), None, b"#")]);
    assert_eq!(send(port, "/many", MULTIPART, &body).body, "note=n.md");
    // A file input left empty sends a part with no file name and no data: it is no file.
    let body = multipart(&[("docs", Some(""), Some("application/octet-stream"), b"")]);
    assert_eq!(send(port, "/many", MULTIPART, &body).body, "no note");
    server.stop(PROGRAM);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn what_clients_send_on_the_wire() {
    let dir = upload_dir("wire");
    let server = start_with_env(PROGRAM, &[("UPLOAD_DIR", dir.to_str().unwrap())]);
    let port = server.port;
    // Lone LF line ends, `filename*`, a delimiter-like line inside the data, a preamble, and
    // fields that repeat.
    let body = "preamble\n--B\nContent-Disposition: form-data; name=\"x\"\n\n1\n--B\nContent-Disposition: form-data; name=\"x\"\n\n2\n--B\nContent-Disposition: form-data; name=\"a\"; filename=\"plain.txt\"; filename*=UTF-8''na%C3%AFve.txt\n\n--Bogus\n--B-\n--B\nContent-Disposition: form-data; name=\"b\"; filename=\"/etc/passwd\"\n\nroot\n--B--\n";
    let response = send(port, "/raw", "multipart/form-data; boundary=B", body.as_bytes());
    assert_eq!(response.status, 200, "{}", response.body);
    assert_eq!(response.body, "fields ['x'] all ['1', '2'] files ['a=naïve.txt:--Bogus\\n--B-', 'b=passwd:root']");
    let unterminated = send(
        port,
        "/raw",
        "multipart/form-data; boundary=B",
        b"--B\r\nContent-Disposition: form-data; name=\"x\"\r\n\r\n1",
    );
    assert_eq!(unterminated.status, 400);
    assert_eq!(unterminated.body, r#"{"error":"the multipart body ends without its closing boundary"}"#);
    assert_eq!(send(port, "/raw", "multipart/form-data", b"").status, 400);
}

#[test]
fn forms_past_each_limit() {
    let dir = upload_dir("limits");
    let server = start_with_env(PROGRAM, &[("UPLOAD_DIR", dir.to_str().unwrap())]);
    let port = server.port;
    // `max_files` is 3.
    let file = |name| (name, Some("f.txt"), None, &b"x"[..]);
    let three = multipart(&[file("docs"), file("docs"), file("note")]);
    assert_eq!(send(port, "/many", MULTIPART, &three).status, 200);
    let four = multipart(&[file("docs"), file("docs"), file("docs"), file("docs")]);
    let refused = send(port, "/many", MULTIPART, &four);
    assert_eq!(refused.status, 413);
    assert_eq!(refused.body, r#"{"error":"the form has more than 3 files"}"#);
    // `max_fields` is 5.
    let urlencoded = "application/x-www-form-urlencoded";
    assert_eq!(send(port, "/raw", urlencoded, b"a=1&a=2&a=3&a=4&a=5").status, 200);
    let refused = send(port, "/raw", urlencoded, b"a=1&a=2&a=3&a=4&a=5&a=6");
    assert_eq!(refused.status, 413);
    assert_eq!(refused.body, r#"{"error":"the form has more than 5 fields"}"#);
    // `max_part_headers` is 300 bytes.
    let long = format!(
        "--B\r\nContent-Disposition: form-data; name=\"a\"\r\nX-Pad: {}\r\n\r\n1\r\n--B--\r\n",
        "p".repeat(300)
    );
    let refused = send(port, "/raw", "multipart/form-data; boundary=B", long.as_bytes());
    assert_eq!(refused.status, 400);
    assert_eq!(refused.body, r#"{"error":"a part's headers are longer than 300 bytes"}"#);
    // `max_body` is 4,000,000 bytes: past it the form is never read.
    let big = vec![b'a'; 4_000_001];
    let body = multipart(&[("avatar", Some("huge.bin"), None, &big)]);
    let refused = send(port, "/avatar", MULTIPART, &body);
    assert_eq!(refused.status, 413);
    assert_eq!(waiting(&dir), Vec::<String>::new());
}

#[test]
fn form_routes_document_their_bodies() {
    let dir = upload_dir("docs");
    let server = start_with_env(PROGRAM, &[("UPLOAD_DIR", dir.to_str().unwrap())]);
    let (mut stream, mut reader) = connect(server.port);
    stream.write_all(b"GET /docs/openapi.json HTTP/1.1\r\nHost: test\r\n\r\n").unwrap();
    let doc = read_response(&mut reader, false).body;
    let signup = r##""requestBody":{"required":true,"content":{"application/x-www-form-urlencoded":{"schema":{"$ref":"#/components/schemas/Signup"}},"multipart/form-data":{"schema":{"$ref":"#/components/schemas/Signup"}}}}"##;
    assert!(doc.contains(signup), "{doc}");
    let avatar = r##""multipart/form-data":{"schema":{"allOf":[{"$ref":"#/components/schemas/Caption"},{"type":"object","properties":{"avatar":{"type":"string","format":"binary"}},"required":["avatar"]}]}}"##;
    assert!(doc.contains(avatar), "{doc}");
    let many = r#""properties":{"docs":{"type":"array","items":{"type":"string","format":"binary"}},"note":{"type":"string","format":"binary"}}}"#;
    assert!(doc.contains(many), "{doc}");
}
