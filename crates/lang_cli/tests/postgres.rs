//! The PostgreSQL client against a real server: `tests/db/database`, compiled and run with the
//! URL in `<PREFIX>TEST_DATABASE_URL` (the prefix is `lang_names::ENV_PREFIX`), and, if it is set,
//! the verify-full URL in `<PREFIX>TEST_DATABASE_TLS_URL`. Without a URL the test passes without
//! running anything, since most machines have no server to spare; tests/db/README.md says how to
//! start a throwaway one.

mod common;

use common::{build_runtime, repo_root};
use lang_names::{CLI_NAME, SOURCE_EXTENSION, env_var};
use std::process::Command;

#[test]
fn postgres_client() {
    let Ok(url) = std::env::var(env_var("TEST_DATABASE_URL")) else {
        eprintln!("skipped: set {} to run the PostgreSQL test", env_var("TEST_DATABASE_URL"));
        return;
    };
    let tls_url = std::env::var(env_var("TEST_DATABASE_TLS_URL")).ok();
    build_runtime();
    let root = repo_root();
    let program = root.join(format!("tests/db/database.{SOURCE_EXTENSION}"));
    let source = std::fs::read_to_string(&program).unwrap();
    let mut expected: String =
        source.lines().filter_map(|l| l.strip_prefix("# expect: ")).map(|l| format!("{l}\n")).collect();
    if tls_url.is_some() {
        expected.extend(source.lines().filter_map(|l| l.strip_prefix("# expect-tls: ")).map(|l| format!("{l}\n")));
    }
    let compiler = std::env::var_os(format!("CARGO_BIN_EXE_{CLI_NAME}")).unwrap();
    let output = Command::new(compiler)
        .arg("run")
        .arg(&program)
        .arg(&url)
        .args(tls_url.iter())
        .current_dir(&root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout == expected,
        "{}\n--- expected\n{expected}--- actual\n{stdout}--- stderr\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}
