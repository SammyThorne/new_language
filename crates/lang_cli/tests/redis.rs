//! The `redis` module (docs/14, milestone 25).
//!
//! - Against a fake Redis that speaks only RESP2, always: the client falls back from `HELLO 3`,
//!   authenticates, picks the database, and a wrong password is refused. The fake records the
//!   commands it was sent, and that the pipeline arrived in one write.
//! - Against a real Redis, when `<PREFIX>TEST_REDIS_URL` (the prefix is `lang_names::ENV_PREFIX`)
//!   names one, such as `redis://127.0.0.1:6379`: every command, pipelines, transactions,
//!   pub/sub, the shared rate limit and the job queue. The test uses its own keys.

mod common;

use common::{build_runtime, compiler, repo_root};
use lang_names::{SOURCE_EXTENSION, env_var};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::sync::{Arc, Mutex};

/// One command from the front of `buf`: its words and the bytes it used; `None` if incomplete.
fn command(buf: &[u8]) -> Option<(Vec<Vec<u8>>, usize)> {
    fn line(buf: &[u8], from: usize) -> Option<(&[u8], usize)> {
        let end = buf[from..].windows(2).position(|w| w == b"\r\n")?;
        Some((&buf[from..from + end], from + end + 2))
    }
    let (head, mut at) = line(buf, 0)?;
    assert_eq!(head.first(), Some(&b'*'), "the client sends arrays");
    let count: usize = std::str::from_utf8(&head[1..]).ok()?.parse().ok()?;
    let mut words = Vec::new();
    for _ in 0..count {
        let (size, next) = line(buf, at)?;
        let size: usize = std::str::from_utf8(&size[1..]).ok()?.parse().ok()?;
        if buf.len() < next + size + 2 {
            return None;
        }
        words.push(buf[next..next + size].to_vec());
        at = next + size + 2;
    }
    Some((words, at))
}

#[derive(Default)]
struct Fake {
    values: HashMap<String, String>,
    hashes: HashMap<String, Vec<(String, String)>>,
    authenticated: bool,
    /// Every command, as text, and the size of each batch that arrived in one read.
    log: Vec<String>,
    batches: Vec<usize>,
}

fn bulk(text: &str) -> String {
    format!("${}\r\n{text}\r\n", text.len())
}

fn answer(fake: &mut Fake, words: &[String]) -> String {
    let name = words[0].to_uppercase();
    let arg = |i: usize| words.get(i).map(String::as_str).unwrap_or("");
    match name.as_str() {
        "HELLO" => format!("-ERR unknown command 'HELLO', with args beginning with: '{}'\r\n", arg(1)),
        "AUTH" => {
            if arg(1) == "s3cret" {
                fake.authenticated = true;
                "+OK\r\n".into()
            } else {
                "-WRONGPASS invalid password\r\n".into()
            }
        }
        _ if !fake.authenticated => "-NOAUTH Authentication required.\r\n".into(),
        "SELECT" => "+OK\r\n".into(),
        "PING" => "+PONG\r\n".into(),
        "SET" => {
            if words.iter().skip(3).any(|w| w.eq_ignore_ascii_case("NX")) && fake.values.contains_key(arg(1)) {
                return "$-1\r\n".into();
            }
            fake.values.insert(arg(1).into(), arg(2).into());
            "+OK\r\n".into()
        }
        "GET" => match fake.values.get(arg(1)) {
            Some(value) => bulk(value),
            None => "$-1\r\n".into(),
        },
        "INCRBY" => {
            let by: i64 = arg(2).parse().unwrap_or(0);
            let now: i64 = fake.values.get(arg(1)).and_then(|v| v.parse().ok()).unwrap_or(0) + by;
            fake.values.insert(arg(1).into(), now.to_string());
            format!(":{now}\r\n")
        }
        "HSET" => {
            let fields = fake.hashes.entry(arg(1).into()).or_default();
            for pair in words[2..].chunks(2) {
                fields.push((pair[0].clone(), pair[1].clone()));
            }
            format!(":{}\r\n", words[2..].len() / 2)
        }
        // RESP2: a flat array of names and values.
        "HGETALL" => {
            let fields = fake.hashes.get(arg(1)).cloned().unwrap_or_default();
            let mut out = format!("*{}\r\n", fields.len() * 2);
            for (name, value) in fields {
                out += &bulk(&name);
                out += &bulk(&value);
            }
            out
        }
        other => format!("-ERR unknown command '{other}'\r\n"),
    }
}

fn serve(stream: TcpStream, fake: Arc<Mutex<Fake>>) {
    let mut stream = stream;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        buf.extend_from_slice(&chunk[..n]);
        let mut out = String::new();
        let mut batch = 0;
        while let Some((words, used)) = command(&buf) {
            buf.drain(..used);
            let words: Vec<String> = words.iter().map(|w| String::from_utf8_lossy(w).into_owned()).collect();
            let mut fake = fake.lock().unwrap();
            fake.log.push(words.join(" "));
            out += &answer(&mut fake, &words);
            batch += 1;
        }
        if batch > 0 {
            fake.lock().unwrap().batches.push(batch);
            if stream.write_all(out.as_bytes()).is_err() {
                return;
            }
        }
    }
}

fn expected_lines(source: &str) -> String {
    source.lines().filter_map(|l| l.strip_prefix("# expect: ")).map(|l| format!("{l}\n")).collect()
}

#[test]
fn a_resp2_server_with_a_password_and_a_database() {
    build_runtime();
    let fake = Arc::new(Mutex::new(Fake::default()));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let shared = Arc::clone(&fake);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let fake = Arc::clone(&shared);
            std::thread::spawn(move || serve(stream, fake));
        }
    });

    let program = repo_root().join(format!("tests/redis/resp2.{SOURCE_EXTENSION}"));
    let expected = expected_lines(&std::fs::read_to_string(&program).unwrap());
    let output = Command::new(compiler())
        .arg("run")
        .arg(&program)
        .arg(format!("redis://:s3cret@127.0.0.1:{port}/3"))
        .arg(format!("redis://:wrong@127.0.0.1:{port}"))
        .current_dir(repo_root())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout == expected,
        "{}\n--- expected\n{expected}--- actual\n{stdout}--- stderr\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let fake = fake.lock().unwrap();
    // The first connection's start: `HELLO` refused, then `AUTH` and `SELECT`.
    assert_eq!(fake.log[..3], ["HELLO 3 AUTH default s3cret", "AUTH s3cret", "SELECT 3"]);
    // The four commands of the pipeline arrived in one read.
    assert!(fake.batches.contains(&4), "no batch of four commands: {:?}", fake.batches);
}

#[test]
fn against_a_real_redis() {
    let Ok(url) = std::env::var(env_var("TEST_REDIS_URL")) else {
        eprintln!("skipped: set {} to run the Redis test", env_var("TEST_REDIS_URL"));
        return;
    };
    build_runtime();
    let program = repo_root().join(format!("tests/redis/real.{SOURCE_EXTENSION}"));
    let expected = expected_lines(&std::fs::read_to_string(&program).unwrap());
    let output = Command::new(compiler()).arg("run").arg(&program).arg(&url).current_dir(repo_root()).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout == expected,
        "{}\n--- expected\n{expected}--- actual\n{stdout}--- stderr\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}
