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
