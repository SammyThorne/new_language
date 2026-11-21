//! What a built program looks like (docs/11, milestone 5): small, because the runtime's debug
//! information is left out when linking, with backtraces that still name the program's own
//! functions and lines; and `explain` prints the error reference's section for a code.

mod common;

use lang_names::{CLI_NAME, SOURCE_EXTENSION, env_var};
use std::path::PathBuf;
use std::process::Command;

fn compiler() -> PathBuf {
    PathBuf::from(std::env::var_os(format!("CARGO_BIN_EXE_{CLI_NAME}")).expect("cargo sets the path of the compiler"))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("executables-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn build(source: &PathBuf, output: &PathBuf, keep_runtime_debug: bool) {
    let mut command = Command::new(compiler());
    command.arg("build").arg(source).arg("-o").arg(output);
    if keep_runtime_debug {
        command.env(env_var("RUNTIME_DEBUG"), "1");
    }
    let status = command.status().unwrap();
    assert!(status.success());
}

#[test]
fn runtime_debug_information_is_left_out() {
    common::build_runtime();
    let source = common::repo_root().join(format!("tests/run/fib.{SOURCE_EXTENSION}"));
    let (small, full) = (scratch("fib_small"), scratch("fib_full"));
    build(&source, &small, false);
    build(&source, &full, true);
    let (small_size, full_size) = (std::fs::metadata(&small).unwrap().len(), std::fs::metadata(&full).unwrap().len());
    // Without `objcopy` nothing is stripped, and the two are the same.
    if small_size == full_size {
        eprintln!("`objcopy` is not available; nothing was stripped");
        return;
    }
    assert!(small_size < 1_500_000, "{small_size} bytes");
    assert!(full_size > 2 * small_size, "{full_size} against {small_size}");
    let run = Command::new(&small).output().unwrap();
    assert!(run.status.success());
}

#[test]
fn backtraces_still_name_the_programs_functions() {
    common::build_runtime();
    let source = scratch(&format!("deep.{SOURCE_EXTENSION}"));
    std::fs::write(
        &source,
        "def inner(x: int) -> int:\n    xs = [1]\n    return xs[x]\n\ndef outer(x: int) -> int:\n    return inner(x) + 1\n\ndef main():\n    print(outer(5))\n",
    )
    .unwrap();
    let program = scratch("deep");
    build(&source, &program, false);
    let run = Command::new(&program).output().unwrap();
    assert_eq!(run.status.code(), Some(101));
    let stderr = String::from_utf8_lossy(&run.stderr);
    for expected in ["backtrace:", "inner at", "outer at", "main at", &format!("deep.{SOURCE_EXTENSION}:3")] {
        assert!(stderr.contains(expected), "{expected} in {stderr}");
    }
}

/// A phrase of the documentation viewer's script, to find out whether a program carries it.
const VIEWER_MARK: &[u8] = b"Filter by path, method, summary or tag";

fn build_release(source: &PathBuf, output: &PathBuf) {
    let status =
        Command::new(compiler()).arg("build").arg("--release").arg(source).arg("-o").arg(output).status().unwrap();
    assert!(status.success());
}

fn carries_viewer(program: &PathBuf) -> bool {
    std::fs::read(program).unwrap().windows(VIEWER_MARK.len()).any(|w| w == VIEWER_MARK)
}

#[test]
fn only_programs_that_serve_documentation_carry_the_viewer() {
    common::build_runtime();
    let root = common::repo_root();
    // Optimised, a server that does not call `docs` leaves the viewer's files out: LLVM drops the
    // unused `docs` code, and then `--gc-sections` the runtime's assets.
    let bench = scratch("bench_server");
    build_release(&root.join(format!("bench/http/server.{SOURCE_EXTENSION}")), &bench);
    assert!(!carries_viewer(&bench), "the benchmark server carries the documentation viewer");

    // The todo service serves it, at a cost of under 100 KB against the same service without.
    let source = std::fs::read_to_string(root.join(format!("tests/http/todo_annotations.{SOURCE_EXTENSION}"))).unwrap();
    let docs_call = source.lines().find(|l| l.contains("server.docs(")).expect("the todo service calls `docs`");
    let without_source = scratch(&format!("todo_plain.{SOURCE_EXTENSION}"));
    std::fs::write(&without_source, source.replace(docs_call, "")).unwrap();
    let (with, without) = (scratch("todo_docs"), scratch("todo_plain"));
    build_release(&root.join(format!("tests/http/todo_annotations.{SOURCE_EXTENSION}")), &with);
    build_release(&without_source, &without);
    assert!(carries_viewer(&with), "the todo service does not carry the viewer");
    assert!(!carries_viewer(&without));
    // With the optimised runtime the documentation costs 101 KB (44 KB of it the viewer's files);
    // the tests link the unoptimised one, whose string and dictionary code is larger, and it
    // costs 169 KB. The bound catches the files or the document code being carried twice.
    let (with_size, without_size) =
        (std::fs::metadata(&with).unwrap().len(), std::fs::metadata(&without).unwrap().len());
    assert!(with_size < without_size + 220_000, "{with_size} bytes with the documentation, {without_size} without");
}

/// A line of the metrics dashboard's page, which only programs that call `metrics` carry.
const DASHBOARD_MARK: &[u8] = b"Percentiles are estimated from histogram buckets";

#[test]
fn only_programs_that_serve_metrics_carry_the_dashboard() {
    common::build_runtime();
    let root = common::repo_root();
    let (plain, metered) = (scratch("todo_no_metrics"), scratch("metrics_server"));
    build_release(&root.join(format!("tests/http/todo_annotations.{SOURCE_EXTENSION}")), &plain);
    build_release(&root.join(format!("tests/http/metrics_server.{SOURCE_EXTENSION}")), &metered);
    let carries =
        |program: &PathBuf| std::fs::read(program).unwrap().windows(DASHBOARD_MARK.len()).any(|w| w == DASHBOARD_MARK);
    assert!(!carries(&plain), "a server without metrics carries the dashboard");
    assert!(carries(&metered), "the metrics server does not carry the dashboard");
}

#[test]
fn explain_prints_a_section_of_the_reference() {
    let ok = Command::new(compiler()).args(["explain", "e0300"]).output().unwrap();
    assert!(ok.status.success());
    let text = String::from_utf8_lossy(&ok.stdout);
    assert!(text.starts_with("E0300: type mismatch"), "{text}");
    assert!(text.contains("What to do."), "{text}");
    let missing = Command::new(compiler()).args(["explain", "E9999"]).output().unwrap();
    assert_eq!(missing.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("no error or warning"));
}
