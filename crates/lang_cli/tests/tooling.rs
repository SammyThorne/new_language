//! Milestone 11's commands, run as a user would: `test`, `fmt --check`, and building a project
//! whose manifest names a git dependency and a path dependency.

use lang_names::{CLI_NAME, DEPS_DIR, LOCK_FILE, MANIFEST_FILE, SOURCE_EXTENSION};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn compiler() -> PathBuf {
    let variable = format!("CARGO_BIN_EXE_{CLI_NAME}");
    PathBuf::from(std::env::var_os(&variable).unwrap_or_else(|| panic!("cargo did not set {variable}")))
}

fn build_runtime() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo).args(["build", "--quiet", "-p", "lang_rt"]).current_dir(root).status().unwrap();
    assert!(status.success());
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("{CLI_NAME}-tooling-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn source(dir: &Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(format!("{name}.{SOURCE_EXTENSION}"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, text).unwrap();
    path
}

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(compiler()).args(args).current_dir(dir).output().unwrap()
}

fn text(output: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

#[test]
fn test_command_runs_each_test_in_its_own_task() {
    build_runtime();
    let dir = scratch("test");
    source(
        &dir,
        "calc",
        "def add(a: int, b: int) -> int:\n    return a + b\n\n@test\ndef adds():\n    assert add(2, 3) == 5\n\n@test\ndef adds_wrong():\n    assert add(2, 2) == 5, f\"got {add(2, 2)}\"\n\n@test\ndef parses() -> Result[None]:\n    n = int.parse(\"12\")?\n    assert n == 12\n\n@test\ndef parse_fails() -> Result[None]:\n    int.parse(\"x\")?\n\ndef main():\n    print(add(1, 1))\n",
    );
    let output = run(&dir, &["test"]);
    let all = text(&output);
    assert!(!output.status.success(), "{all}");
    for line in [
        "test adds ... ok",
        "test adds_wrong ... FAILED",
        "test parses ... ok",
        "test parse_fails ... FAILED",
        "got 4",
        "is not a valid int",
        "2 passed; 2 failed",
    ] {
        assert!(all.contains(line), "missing {line:?} in:\n{all}");
    }
    let output = run(&dir, &["test", "--filter", "parses"]);
    assert!(output.status.success() && text(&output).contains("1 passed; 0 failed"), "{}", text(&output));
    // The file is still an ordinary program.
    let output = run(&dir, &["run", &format!("calc.{SOURCE_EXTENSION}")]);
    assert_eq!(String::from_utf8_lossy(&output.stdout), "2\n");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn build_is_the_default_command() {
    build_runtime();
    let dir = scratch("default");
    source(&dir, "app", "def main():\n    print(\"built\")\n");
    let file = format!("app.{SOURCE_EXTENSION}");
    for args in [vec![file.as_str(), "-o", "short"], vec!["--release", file.as_str(), "-o", "short_release"]] {
        let output = run(&dir, &args);
        assert!(output.status.success(), "{args:?}: {}", text(&output));
        let program = Command::new(dir.join(args.last().unwrap())).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&program.stdout), "built\n");
    }
    // Commands, help and mistakes are not taken for a file to build.
    assert!(run(&dir, &["check", &file]).status.success());
    assert!(text(&run(&dir, &["--help"])).contains("build"));
    let typo = run(&dir, &["biuld"]);
    assert!(!typo.status.success());
    assert!(text(&typo).contains("unrecognized subcommand"), "{}", text(&typo));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn fmt_check_and_rewrite() {
    let dir = scratch("fmt");
    let path = source(&dir, "messy", "def f( x:int )->int:\n  return x*2\n");
    let output = run(&dir, &["fmt", "--check"]);
    assert!(!output.status.success() && text(&output).contains("messy"), "{}", text(&output));
    assert!(run(&dir, &["fmt"]).status.success());
    assert_eq!(fs::read_to_string(&path).unwrap(), "def f(x: int) -> int:\n    return x * 2\n");
    assert!(run(&dir, &["fmt", "--check"]).status.success());
    let _ = fs::remove_dir_all(dir);
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "user.name=test", "-c", "user.email=test@example.invalid", "-c", "init.defaultBranch=main"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(status.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&status.stderr));
}

#[test]
fn git_and_path_dependencies() {
    build_runtime();
    let root = scratch("deps");
    // A library in a local git repository, with two versions tagged.
    let library = root.join("textutil");
    fs::create_dir_all(&library).unwrap();
    git(&library, &["init", "--quiet"]);
    source(
        &library,
        "lib",
        "import textutil.shout as shout\n\npub def greet(name: str) -> str:\n    return shout.loud(f\"hello {name}\")\n",
    );
    source(&library, "shout", "pub def loud(text: str) -> str:\n    return text.upper()\n");
    git(&library, &["add", "."]);
    git(&library, &["commit", "--quiet", "-m", "v1"]);
    git(&library, &["tag", "v1"]);
    source(&library, "shout", "pub def loud(text: str) -> str:\n    return text.upper() + \"!\"\n");
    git(&library, &["commit", "--quiet", "-am", "v2"]);
    // A path dependency.
    source(&root.join("shared"), "lib", "pub const ANSWER: int = 42\n");
    // The project.
    let project = root.join("app");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join(MANIFEST_FILE),
        format!("[package]\nname = \"app\"\n\n[dependencies]\ntextutil = {{ git = \"file://{}\", rev = \"v1\" }}\nshared = {{ path = \"../shared\" }}\n", library.display()),
    )
    .unwrap();
    source(
        &project,
        "main",
        "import textutil\nimport shared\n\ndef main():\n    print(textutil.greet(\"ann\"), shared.ANSWER)\n",
    );
    let main = format!("main.{SOURCE_EXTENSION}");
    let output = run(&project, &["run", &main]);
    assert_eq!(String::from_utf8_lossy(&output.stdout), "HELLO ANN 42\n", "{}", text(&output));
    assert!(project.join(DEPS_DIR).join("textutil").is_dir());
    let lock = fs::read_to_string(project.join(LOCK_FILE)).unwrap();
    assert!(lock.contains("textutil = \""), "{lock}");
    // The lock file keeps the build at v1 even when the manifest's rev moves on.
    let manifest = fs::read_to_string(project.join(MANIFEST_FILE)).unwrap().replace("rev = \"v1\"", "rev = \"main\"");
    fs::write(project.join(MANIFEST_FILE), manifest).unwrap();
    assert_eq!(String::from_utf8_lossy(&run(&project, &["run", &main]).stdout), "HELLO ANN 42\n");
    // `fetch --update` moves to the manifest's rev.
    assert!(run(&project, &["fetch", &main, "--update"]).status.success());
    assert_eq!(String::from_utf8_lossy(&run(&project, &["run", &main]).stdout), "HELLO ANN! 42\n");
    let _ = fs::remove_dir_all(root);
}
