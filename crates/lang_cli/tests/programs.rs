//! End-to-end tests against the real compiler executable.
//!
//! - `tests/run/*`: each program is compiled and run in debug and release mode. Its standard
//!   output must equal the `# expect:` comment lines in the file, in order. `# args:` gives it
//!   command-line arguments and `# env: NAME=value` sets an environment variable. Modules it imports live in subdirectories, which are not run.
//! - `tests/ui/*`: each program is checked, and the compiler's standard error must equal the
//!   `.stderr` file next to it. The expected exit status is 1 if that file contains an error,
//!   otherwise 0. Set the `BLESS` environment variable (with the language's prefix, see
//!   `lang_names::env_var`) to rewrite the `.stderr` files from the current output.

use lang_names::{CLI_NAME, SOURCE_EXTENSION, env_var};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

/// Cargo tells integration tests where it built each binary through `CARGO_BIN_EXE_<name>`,
/// both at compile time and when the tests run.
fn compiler() -> PathBuf {
    let variable = format!("CARGO_BIN_EXE_{CLI_NAME}");
    let path = PathBuf::from(std::env::var_os(&variable).unwrap_or_else(|| panic!("cargo did not set {variable}")));
    assert!(path.is_file(), "compiler not found at {}", path.display());
    ensure_runtime_built();
    path
}

/// `cargo test` builds the runtime crate only as a test harness, never as the static library
/// that compiled programs link against, so build that once here. It lands next to the
/// compiler, where the compiler looks for it.
fn ensure_runtime_built() {
    static BUILT: OnceLock<()> = OnceLock::new();
    BUILT.get_or_init(|| {
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let mut command = Command::new(cargo);
        command.args(["build", "--quiet", "-p", "lang_rt"]).current_dir(repo_root());
        if !cfg!(debug_assertions) {
            command.arg("--release");
        }
        let status = command.status().expect("could not run cargo to build the runtime");
        assert!(status.success(), "building the runtime library failed");
    });
}

fn compile(args: &[&str]) -> Output {
    Command::new(compiler()).args(args).current_dir(repo_root()).output().unwrap()
}

/// Source files in a directory under the repository root, as paths relative to the root.
fn programs(dir: &str) -> Vec<String> {
    let mut found: Vec<String> = fs::read_dir(repo_root().join(dir))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == SOURCE_EXTENSION))
        .map(|path| format!("{dir}/{}", path.file_name().unwrap().to_string_lossy()))
        .collect();
    found.sort();
    assert!(!found.is_empty(), "no programs in {dir}");
    found
}

/// What a test program declares about its run, in comment lines at the top of the file.
struct Expectations {
    stdout: String,
    exit: i32,
    /// Lines that must each appear in standard error.
    stderr: Vec<String>,
    /// Command-line arguments for the program, from `# args:`.
    args: Vec<String>,
    /// Environment variables for the program, from `# env: NAME=value`.
    env: Vec<(String, String)>,
}

fn expectations(source: &str) -> Expectations {
    let directive = |prefix: &str| -> Vec<String> {
        source
            .lines()
            .filter_map(|line| line.strip_prefix(prefix))
            .map(|rest| rest.strip_prefix(' ').unwrap_or(rest).to_string())
            .collect()
    };
    Expectations {
        stdout: directive("# expect:").iter().map(|line| format!("{line}\n")).collect(),
        exit: directive("# expect-exit:").first().map_or(0, |code| code.trim().parse().expect("exit code")),
        stderr: directive("# expect-stderr:"),
        args: directive("# args:").iter().flat_map(|line| line.split_whitespace().map(String::from)).collect(),
        env: directive("# env:")
            .iter()
            .map(|line| line.split_once('=').expect("# env: NAME=value"))
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect(),
    }
}

/// Every program runs in debug and release builds, and once more in each with the collector
/// running before every allocation, which turns a missing root into an immediate crash. The
/// release run matters as much as the debug one: optimised code keeps fewer values alive, which
/// is how a reference held only by Rust heap memory was found (docs/09, phase 1).
#[test]
fn run_programs() {
    let stress = env_var("GC_STRESS");
    let mut failures = Vec::new();
    for program in programs("tests/run") {
        let expected = expectations(&fs::read_to_string(repo_root().join(&program)).unwrap());
        for (mode, gc_stress) in
            [("debug", false), ("release", false), ("debug, GC stress", true), ("release, GC stress", true)]
        {
            let mut command = Command::new(compiler());
            command.arg("run").current_dir(repo_root());
            if mode.starts_with("release") {
                command.arg("--release");
            }
            if gc_stress {
                command.env(&stress, "1");
            }
            command.envs(expected.env.iter().map(|(name, value)| (name, value)));
            let output = command.arg(&program).args(&expected.args).output().unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let missing: Vec<&String> = expected.stderr.iter().filter(|line| !stderr.contains(line.as_str())).collect();
            if output.status.code() != Some(expected.exit) || stdout != expected.stdout || !missing.is_empty() {
                failures.push(format!(
                    "{program} ({mode}): {}, expected exit {}\n--- expected stdout\n{}--- actual stdout\n{stdout}--- stderr\n{stderr}--- missing from stderr: {missing:?}",
                    output.status, expected.exit, expected.stdout
                ));
            }
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn ui_programs() {
    let bless = std::env::var_os(env_var("BLESS")).is_some();
    let mut failures = Vec::new();
    for program in programs("tests/ui") {
        let output = compile(&["check", &program]);
        let stderr = String::from_utf8(output.stderr).unwrap();
        let golden = repo_root().join(&program).with_extension("stderr");
        if bless {
            fs::write(&golden, &stderr).unwrap();
            continue;
        }
        let Ok(expected) = fs::read_to_string(&golden) else {
            failures.push(format!("{program}: missing {}; set {} to create it", golden.display(), env_var("BLESS")));
            continue;
        };
        let expected_code = if expected.lines().any(|line| line.starts_with("error")) { 1 } else { 0 };
        if stderr != expected || output.status.code() != Some(expected_code) {
            failures.push(format!(
                "{program}: exit {:?}, expected {expected_code}\n--- expected stderr\n{expected}--- actual stderr\n{stderr}",
                output.status.code()
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn emit_intermediate_forms() {
    let hello = format!("tests/run/hello.{SOURCE_EXTENSION}");
    let tokens = compile(&["build", "--emit=tokens", &hello]);
    assert!(tokens.status.success());
    let tokens = String::from_utf8(tokens.stdout).unwrap();
    assert!(tokens.contains("3:1 def\n") && tokens.contains("INDENT") && tokens.contains("DEDENT"), "{tokens}");

    let hir = String::from_utf8(compile(&["build", "--emit=hir", &hello]).stdout).unwrap();
    assert!(hir.contains("op: Print"), "{hir}");

    let ir = String::from_utf8(compile(&["build", "--emit=llvm-ir", &hello]).stdout).unwrap();
    assert!(ir.contains("define void @lang_user_main()"), "{ir}");
}

#[test]
fn exit_status_of_a_failed_compile() {
    let output = compile(&["run", &format!("tests/ui/type_mismatch.{SOURCE_EXTENSION}")]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
}

#[test]
fn rejects_other_file_types() {
    let output = compile(&["check", "Cargo.toml"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains(&format!("expected a `.{SOURCE_EXTENSION}` file")), "{stderr}");
}
