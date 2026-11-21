//! Request IDs and structured log lines (docs/14, milestone 1): `tests/http/request_log` is
//! served with its standard error piped, so its log lines are buffered while it serves, and
//! this test checks what reaches the pipe, in text and in JSON.

mod common;

use common::json::{self, Json};
use common::{compiler, connect, read_response, repo_root};
use lang_names::{CLI_NAME, SOURCE_EXTENSION, env_var};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Run {
    /// Standard error after `listening on`, as it arrives.
    stderr: Arc<Mutex<String>>,
    port: u16,
    child: std::process::Child,
    executable: std::path::PathBuf,
}

impl Drop for Run {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.executable);
    }
}

fn start(format: Option<&str>) -> Run {
    let executable =
        std::env::temp_dir().join(format!("{CLI_NAME}-log-test-{}-{}", format.unwrap_or("text"), std::process::id()));
    let output = Command::new(compiler())
        .args(["build", &format!("tests/http/request_log.{SOURCE_EXTENSION}"), "-o"])
        .arg(&executable)
        .current_dir(repo_root())
        .output()
        .unwrap();
    assert!(output.status.success(), "did not compile:\n{}", String::from_utf8_lossy(&output.stderr));
    let mut command = Command::new(&executable);
    command.env_remove(env_var("LOG")).env_remove(env_var("LOG_FORMAT"));
    if let Some(format) = format {
        command.env(env_var("LOG_FORMAT"), format);
    }
    let mut child = command.stderr(Stdio::piped()).stdout(Stdio::null()).spawn().unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut before = String::new();
    let port = loop {
        let mut line = String::new();
        assert!(stderr.read_line(&mut line).unwrap() > 0, "no `listening on` line in {before:?}");
        if let Some(port) = line.trim().strip_prefix("listening on ") {
            break port.parse().unwrap();
        }
        before.push_str(&line);
    };
    let collected = Arc::new(Mutex::new(before));
    let sink = collected.clone();
    std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        while let Ok(n) = stderr.read(&mut chunk) {
            if n == 0 {
                break;
            }
            sink.lock().unwrap().push_str(&String::from_utf8_lossy(&chunk[..n]));
        }
    });
    Run { stderr: collected, port, child, executable }
}

fn get(run: &Run, path: &str, headers: &str) -> common::Response {
    let (mut stream, mut reader) = connect(run.port);
    write!(stream, "GET {path} HTTP/1.1\r\nHost: test\r\n{headers}\r\n").unwrap();
    read_response(&mut reader, false)
}

/// Wait until standard error holds `text`: buffered lines must arrive without the program
/// ending, within the accept loop's 200 ms (and some slack for a loaded machine).
fn wait_for(run: &Run, text: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !run.stderr.lock().unwrap().contains(text) {
        assert!(Instant::now() < deadline, "{text:?} never reached standard error:\n{}", run.stderr.lock().unwrap());
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Send the script, stop the program with SIGTERM, and return every line it logged.
fn exercise(run: &mut Run, id_marker: &str) -> (Vec<String>, String) {
    let work = get(run, "/work", "X-Request-Id: abc-123\r\nX-Forwarded-For: 198.51.100.7, 203.0.113.9\r\n");
    assert_eq!(work.status, 200);
    assert_eq!(work.header("x-request-id"), Some("abc-123"));
    wait_for(run, "/work");

    // An ID a client may not choose is replaced.
    let replaced = get(run, "/work", "X-Request-Id: no spaces!\r\n");
    let new_id = replaced.header("x-request-id").unwrap().to_string();
    assert!(new_id.len() == 16 && new_id.bytes().all(|b| b.is_ascii_hexdigit()), "{new_id}");

    // A handler's error still carries the ID; a panic ends only its request.
    let failed = get(run, "/fail", "");
    assert_eq!(failed.status, 503);
    assert!(failed.header("x-request-id").is_some());
    assert_eq!(get(run, "/boom", "").status, 500);
    assert_eq!(get(run, "/plain", "").status, 200);

    let status = Command::new("kill").args(["-TERM", &run.child.id().to_string()]).status().unwrap();
    assert!(status.success());
    assert!(run.child.wait().unwrap().success());
    // The reader thread sees the end of the pipe.
    wait_for(run, id_marker);
    let text = run.stderr.lock().unwrap().clone();
    (text.lines().filter(|l| !l.trim().is_empty()).map(str::to_string).collect(), new_id)
}

fn line_with<'a>(lines: &'a [String], text: &str) -> &'a str {
    lines.iter().find(|l| l.contains(text)).unwrap_or_else(|| panic!("no line with {text:?} in {lines:#?}"))
}

#[test]
fn text_lines_carry_fields_and_the_request_id() {
    let mut run = start(None);
    let (lines, new_id) = exercise(&mut run, "INFO  stopped");
    assert!(line_with(&lines, "starting").ends_with("INFO  starting component=setup version=1"), "{lines:#?}");
    assert!(
        line_with(&lines, "in handler").ends_with("in handler request_id=abc-123 user=\"Ann Lee\" client=203.0.113.9"),
        "{lines:#?}"
    );
    assert!(line_with(&lines, "in task").ends_with("in task request_id=abc-123 note=\"from the handler\""));
    let access = line_with(&lines, "GET /work 200 request_id=abc-123");
    assert!(access.contains(" method=GET path=/work status=200 ms="), "{access}");
    assert!(access.ends_with(" client=203.0.113.9"), "{access}");
    assert!(line_with(&lines, &format!("GET /work 200 request_id={new_id}")).ends_with("client=127.0.0.1"));
    assert!(line_with(&lines, "GET /fail 503").contains("ERROR GET /fail 503 request_id="));
    // After the panic, the skipped route logs without an ID.
    assert!(line_with(&lines, "INFO  plain").ends_with("INFO  plain"), "{lines:#?}");
    // Every line came out, in order, the last when the program ended.
    let order: Vec<usize> =
        ["starting", "in handler", "in task", "GET /work 200 request_id=abc-123", "boom", "INFO  plain", "stopped"]
            .iter()
            .map(|t| lines.iter().position(|l| l.contains(t)).unwrap())
            .collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "out of order: {lines:#?}");
}

#[test]
fn json_lines_are_objects_with_the_same_fields() {
    let mut run = start(Some("json"));
    let (lines, _) = exercise(&mut run, "\"msg\":\"stopped\"");
    let objects: Vec<Json> = lines
        .iter()
        .filter(|l| l.starts_with('{'))
        .map(|l| json::parse(l).unwrap_or_else(|e| panic!("{l}: {e}")))
        .collect();
    let find = |msg: &str| -> &Json {
        objects
            .iter()
            .find(|o| o.get("msg").and_then(Json::as_str) == Some(msg))
            .unwrap_or_else(|| panic!("no {msg:?} in {lines:#?}"))
    };
    let field = |o: &Json, key: &str| o.get(key).and_then(Json::as_str).map(str::to_string);
    let handler = find("in handler");
    assert_eq!(handler.keys(), ["time", "level", "msg", "request_id", "user", "client"]);
    assert_eq!(field(handler, "request_id").as_deref(), Some("abc-123"));
    assert_eq!(field(handler, "user").as_deref(), Some("Ann Lee"));
    assert_eq!(field(find("in task"), "request_id").as_deref(), Some("abc-123"));
    let access = find("GET /work 200");
    assert_eq!(field(access, "status").as_deref(), Some("200"));
    assert_eq!(field(access, "client").as_deref(), Some("203.0.113.9"));
    assert_eq!(field(find("GET /fail 503"), "level").as_deref(), Some("error"));
    assert_eq!(find("plain").get("request_id"), None);
    assert_eq!(field(find("starting"), "component").as_deref(), Some("setup"));
}
