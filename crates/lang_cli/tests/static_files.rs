//! `server.static` over a real socket (docs/14, milestone 3): requests that try to leave the root
//! (by `..` in its spellings, encoded separators, NUL, symlinks, dotfiles and very long paths) all
//! fail without the file's contents; HEAD, ranges and 304s keep the connection usable; and a
//! 20 MiB file downloaded several times is streamed, so neither the collected heap nor the
//! process's resident memory grows with it.

mod common;

use common::{build_runtime, connect, start_with_env};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;

const BIG: usize = 20 << 20;
const SECRET: &str = "outside the root";

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }
}

fn send(stream: &mut TcpStream, method: &str, path: &str, extra: &[(&str, &str)]) {
    let mut text = format!("{method} {path} HTTP/1.1\r\nHost: test\r\n");
    for (name, value) in extra {
        text.push_str(&format!("{name}: {value}\r\n"));
    }
    text.push_str("\r\n");
    stream.write_all(text.as_bytes()).unwrap();
}

/// Read one response, with its body as bytes; HEAD and 304 responses have none.
fn receive(reader: &mut BufReader<TcpStream>, method: &str) -> Reply {
    let mut status_line = String::new();
    reader.read_line(&mut status_line).unwrap();
    let status: u16 =
        status_line.split(' ').nth(1).unwrap_or_else(|| panic!("bad status line {status_line:?}")).parse().unwrap();
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':').unwrap();
        headers.push((name.to_ascii_lowercase(), value.trim().to_string()));
    }
    let length: usize = headers.iter().find(|(n, _)| n == "content-length").map_or(0, |(_, v)| v.parse().unwrap());
    let length = if method == "HEAD" || status == 304 { 0 } else { length };
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).unwrap();
    Reply { status, headers, body }
}

fn ask(
    stream: &mut TcpStream,
    reader: &mut BufReader<TcpStream>,
    method: &str,
    path: &str,
    extra: &[(&str, &str)],
) -> Reply {
    send(stream, method, path, extra);
    receive(reader, method)
}

fn big_byte(i: usize) -> u8 {
    (i % 251) as u8
}

/// The server's resident memory, in KiB.
fn resident_kib(pid: u32) -> u64 {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    let line = status.lines().find(|l| l.starts_with("VmRSS:")).unwrap();
    line.split_whitespace().nth(1).unwrap().parse().unwrap()
}

fn make_root(dir: &Path) -> std::path::PathBuf {
    let _ = std::fs::remove_dir_all(dir);
    let root = dir.join("root");
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::create_dir_all(dir.join("outside")).unwrap();
    std::fs::write(dir.join("outside/secret.txt"), SECRET).unwrap();
    std::fs::write(root.join("index.html"), "<h1>home</h1>").unwrap();
    std::fs::write(root.join("hello.txt"), "hello world").unwrap();
    std::fs::write(root.join("docs/index.html"), "<h1>docs</h1>").unwrap();
    std::fs::write(root.join(".env"), SECRET).unwrap();
    std::os::unix::fs::symlink("../outside/secret.txt", root.join("escape.txt")).unwrap();
    std::os::unix::fs::symlink("../outside", root.join("out")).unwrap();
    std::os::unix::fs::symlink("hello.txt", root.join("alias.txt")).unwrap();
    let big: Vec<u8> = (0..BIG).map(big_byte).collect();
    std::fs::write(root.join("big.bin"), big).unwrap();
    root
}

#[test]
fn static_files_over_a_socket() {
    build_runtime();
    let dir = std::env::temp_dir().join(format!("static-files-test-{}", std::process::id()));
    let root = make_root(&dir);
    let alloc_stats = lang_names::env_var("ALLOC_STATS");
    let mut server = start_with_env("static_server", &[("STATIC_ROOT", root.to_str().unwrap()), (&alloc_stats, "1")]);
    let (mut stream, mut reader) = connect(server.port);

    // Escapes. The router may refuse some before the file server sees them; either way, no
    // success and no secret.
    let long = format!("/{}", "a/".repeat(3000));
    let escapes = [
        "/../outside/secret.txt",
        "/%2e%2e/outside/secret.txt",
        "/%2E%2E/outside/secret.txt",
        "/..%2Foutside%2Fsecret.txt",
        "/..%5Coutside%5Csecret.txt",
        "/docs/..%2F..%2Foutside/secret.txt",
        "/hello.txt%00.html",
        "//outside/secret.txt",
        "/escape.txt",
        "/out/secret.txt",
        "/.env",
        "/%2Eenv",
        &long,
    ];
    for path in escapes {
        let (mut stream, mut reader) = connect(server.port);
        send(&mut stream, "GET", path, &[]);
        let reply = receive(&mut reader, "GET");
        assert!(matches!(reply.status, 400 | 404 | 414 | 431), "{path}: status {}", reply.status);
        assert!(!String::from_utf8_lossy(&reply.body).contains(SECRET), "{path} leaked the file");
    }

    // A symlink that stays inside the root is followed; a directory without its slash moves.
    let reply = ask(&mut stream, &mut reader, "GET", "/alias.txt", &[]);
    assert_eq!((reply.status, reply.body.as_slice()), (200, b"hello world".as_slice()));
    let reply = ask(&mut stream, &mut reader, "GET", "/docs", &[]);
    assert_eq!((reply.status, reply.header("location")), (301, Some("/docs/")));
    let reply = ask(&mut stream, &mut reader, "GET", "/docs/", &[]);
    assert_eq!((reply.status, reply.body.as_slice()), (200, b"<h1>docs</h1>".as_slice()));

    // HEAD sends the length and no body; the next answer on the connection is intact.
    let reply = ask(&mut stream, &mut reader, "HEAD", "/hello.txt", &[]);
    assert_eq!((reply.status, reply.header("content-length")), (200, Some("11")));
    assert_eq!(reply.header("content-type"), Some("text/plain; charset=utf-8"));
    let etag = reply.header("etag").unwrap().to_string();
    let reply = ask(&mut stream, &mut reader, "GET", "/hello.txt", &[("Range", "bytes=6-")]);
    assert_eq!((reply.status, reply.body.as_slice()), (206, b"world".as_slice()));
    assert_eq!(reply.header("content-range"), Some("bytes 6-10/11"));

    // A 304 has no body, and the connection carries on.
    let reply = ask(&mut stream, &mut reader, "GET", "/hello.txt", &[("If-None-Match", &etag)]);
    assert_eq!((reply.status, reply.header("etag")), (304, Some(etag.as_str())));
    let reply = ask(&mut stream, &mut reader, "GET", "/", &[]);
    assert_eq!((reply.status, reply.body.as_slice()), (200, b"<h1>home</h1>".as_slice()));

    // A range of the big file larger than the in-memory limit, then a HEAD of the whole file.
    let reply = ask(&mut stream, &mut reader, "GET", "/big.bin", &[("Range", "bytes=1000000-3999999")]);
    assert_eq!((reply.status, reply.body.len()), (206, 3_000_000));
    assert!(reply.body.iter().enumerate().all(|(i, &b)| b == big_byte(1_000_000 + i)), "wrong range bytes");
    let reply = ask(&mut stream, &mut reader, "HEAD", "/big.bin", &[]);
    assert_eq!(reply.header("content-length"), Some(BIG.to_string().as_str()));

    // A client that goes away mid-file does not harm the server.
    {
        let (mut stream, mut reader) = connect(server.port);
        send(&mut stream, "GET", "/big.bin", &[]);
        let mut some = [0u8; 4096];
        reader.read_exact(&mut some).unwrap();
    }

    // The whole file, several times: memory stays flat after the first.
    let reply = ask(&mut stream, &mut reader, "GET", "/big.bin", &[]);
    assert_eq!((reply.status, reply.body.len()), (200, BIG));
    let first = resident_kib(server.child.id());
    for _ in 0..5 {
        let reply = ask(&mut stream, &mut reader, "GET", "/big.bin", &[]);
        assert_eq!(reply.body.len(), BIG);
        assert!(reply.body.iter().enumerate().all(|(i, &b)| b == big_byte(i)), "wrong file bytes");
    }
    let last = resident_kib(server.child.id());
    assert!(last < first + 4096, "resident memory grew from {first} KiB to {last} KiB");
    assert!(last < 20 * 1024, "resident memory {last} KiB holds the file");

    drop((stream, reader));
    server.stop("static_server");
    let stderr = server.stderr.lock().unwrap().clone();
    let heap: usize = stderr
        .split_whitespace()
        .find_map(|w| w.strip_prefix("heap="))
        .unwrap_or_else(|| panic!("no allocation stats in {stderr:?}"))
        .parse()
        .unwrap();
    assert!(heap < 16 << 20, "the collected heap grew to {heap} bytes");
    let _ = std::fs::remove_dir_all(&dir);
}
