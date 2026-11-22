//! SQLite across processes and connections (docs/14, milestone 17): a file opens in WAL mode with
//! a busy timeout and foreign keys on; two processes, each with two connections and four tasks,
//! write to one file at once and every row arrives; and a program that opens a database does not
//! link `libsqlite3` (it is loaded when the first database opens), so one that does not import
//! `sqlite` is no different.

mod common;

use common::{build_runtime, compiler, repo_root};
use lang_names::SOURCE_EXTENSION;
use std::process::{Command, Stdio};

fn build(name: &str, dir: &std::path::Path) -> std::path::PathBuf {
    let source = repo_root().join(format!("tests/db/{name}.{SOURCE_EXTENSION}"));
    let exe = dir.join(name);
    let output = Command::new(compiler())
        .arg("build")
        .arg(&source)
        .arg("-o")
        .arg(&exe)
        .current_dir(repo_root())
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    exe
}

fn run(exe: &std::path::Path, args: &[&str]) -> String {
    let output = Command::new(exe).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
fn processes_share_a_database_file() {
    build_runtime();
    let dir = std::env::temp_dir().join(format!("sqlite-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let exe = build("sqlite_writers", &dir);
    let file = dir.join("shared.db");
    let file = file.to_str().unwrap();
    assert_eq!(run(&exe, &["setup", file]), "wal\n1\n5000");
    let writers: Vec<_> = ["a", "b"]
        .iter()
        .map(|who| {
            Command::new(&exe).args(["write", file, who]).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap()
        })
        .collect();
    for writer in writers {
        let output = writer.wait_with_output().unwrap();
        assert!(
            output.status.success() && output.stdout == b"written\n",
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(run(&exe, &["count", file]), "600");

    // The library is loaded when needed, not linked.
    let libraries = Command::new("readelf").args(["-d"]).arg(&exe).output();
    if let Ok(libraries) = libraries {
        let text = String::from_utf8_lossy(&libraries.stdout);
        assert!(!text.contains("sqlite"), "{text}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
