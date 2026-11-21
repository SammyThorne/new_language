//! Helpers shared by the tests that run the server programs in `tests/http/`: compile one, start
//! it on a free port, talk HTTP to it, and stop it.

#![allow(dead_code)]

pub mod json;

use lang_names::{CLI_NAME, SOURCE_EXTENSION, env_var};
use std::io::{BufRead, BufReader, Read};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

pub fn compiler() -> PathBuf {
    let variable = format!("CARGO_BIN_EXE_{CLI_NAME}");
    PathBuf::from(std::env::var_os(&variable).unwrap_or_else(|| panic!("cargo did not set {variable}")))
}

pub fn build_runtime() {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status =
        Command::new(cargo).args(["build", "--quiet", "-p", "lang_rt"]).current_dir(repo_root()).status().unwrap();
    assert!(status.success(), "building the runtime library failed");
}

/// A response: status, lower-cased headers, and body.
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }
}

/// Read one response from a keep-alive connection.
pub fn read_response(reader: &mut BufReader<TcpStream>, head_request: bool) -> Response {
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
    let mut body = vec![0u8; if head_request { 0 } else { length }];
    reader.read_exact(&mut body).unwrap();
    Response { status, headers, body: String::from_utf8(body).unwrap() }
}

pub struct Server {
    pub child: Child,
    pub port: u16,
    /// Standard error after the `listening on` line, as it arrives.
    pub stderr: std::sync::Arc<std::sync::Mutex<String>>,
    executable: PathBuf,
}

impl Server {
    /// Send SIGTERM and check that the server shuts down cleanly.
    pub fn stop(&mut self, program: &str) {
        let status = Command::new("kill").args(["-TERM", &self.child.id().to_string()]).status().unwrap();
        assert!(status.success());
        let exit = self.child.wait().unwrap();
        assert!(exit.success(), "{program} did not shut down cleanly: {exit}");
    }
}

impl Drop for Server {
    /// Stop the server (if a failed assertion left it running) and remove its executable.
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.executable);
    }
}

/// Compile `tests/http/<program>` and start it. The program prints `listening on <port>` to
/// standard error once it is ready.
pub fn start(program: &str) -> Server {
    start_with_env(program, &[])
}

/// [`start`], with environment variables for the program.
pub fn start_with_env(program: &str, env: &[(&str, &str)]) -> Server {
    let root = repo_root();
    // Tests of one file run at once, several of them perhaps with the same program.
    static STARTED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = STARTED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let executable = std::env::temp_dir().join(format!("{CLI_NAME}-http-test-{program}-{}-{n}", std::process::id()));
    let output = Command::new(compiler())
        .args(["build", &format!("tests/http/{program}.{SOURCE_EXTENSION}"), "-o"])
        .arg(&executable)
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(output.status.success(), "{program} did not compile:\n{}", String::from_utf8_lossy(&output.stderr));
    let mut child = Command::new(&executable)
        .current_dir(&root)
        .env(env_var("LOG"), "warn")
        .envs(env.iter().copied())
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut line = String::new();
    stderr.read_line(&mut line).unwrap();
    let port = line
        .trim()
        .strip_prefix("listening on ")
        .unwrap_or_else(|| panic!("unexpected output {line:?}"))
        .parse()
        .unwrap();
    // Keep reading standard error so the server never blocks on it.
    let collected = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let sink = collected.clone();
    std::thread::spawn(move || {
        let mut line = String::new();
        while stderr.read_line(&mut line).is_ok_and(|n| n > 0) {
            sink.lock().unwrap().push_str(&line);
            line.clear();
        }
    });
    Server { child, port, stderr: collected, executable }
}

pub fn connect(port: u16) -> (TcpStream, BufReader<TcpStream>) {
    let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let reader = BufReader::new(stream.try_clone().unwrap());
    (stream, reader)
}
