//! The documentation page's script (docs/12, milestone 4): `core.js` holds what the page works
//! out (examples from schemas, requests, curl commands, safe Markdown), and `core.test.js` tests
//! it with Node's built-in runner. Node is not needed to build or use the language, so without it
//! the tests are skipped with a notice.

mod common;

use std::process::Command;

#[test]
fn viewer_core_passes_its_tests() {
    let Ok(version) = Command::new("node").arg("--version").output() else {
        eprintln!("node is not installed; the viewer's script tests are skipped");
        return;
    };
    let version = String::from_utf8_lossy(&version.stdout).trim().to_string();
    // `node --test` with a file argument came in Node 18.
    let major: u32 = version.trim_start_matches('v').split('.').next().and_then(|m| m.parse().ok()).unwrap_or(0);
    if major < 18 {
        eprintln!("node {version} is older than 18; the viewer's script tests are skipped");
        return;
    }
    let tests = common::repo_root().join("crates/lang_rt/assets/viewer/core.test.js");
    let run = Command::new("node").arg("--test").arg(&tests).output().unwrap();
    assert!(run.status.success(), "{}\n{}", String::from_utf8_lossy(&run.stdout), String::from_utf8_lossy(&run.stderr));
}

/// The page in a real browser, when `<PREFIX>VIEWER_BROWSER` names a Chromium executable: the
/// todo service's page is loaded headless, its scripts run, and the page they built must list
/// every operation with no script error. CI does not set it; `tests/http/docs_page_check.md` is
/// the manual check that covers the rest.
#[test]
fn page_builds_in_a_browser() {
    let Some(browser) = std::env::var_os(lang_names::env_var("VIEWER_BROWSER")) else {
        eprintln!("{} is not set; the page is not checked in a browser", lang_names::env_var("VIEWER_BROWSER"));
        return;
    };
    common::build_runtime();
    let program = "todo_annotations";
    let mut server = common::start(program);
    let run = Command::new(browser)
        .args(["--headless", "--no-sandbox", "--disable-gpu", "--enable-logging=stderr", "--v=0"])
        .arg("--virtual-time-budget=5000")
        .arg("--dump-dom")
        .arg(format!("http://127.0.0.1:{}/docs", server.port))
        .output()
        .unwrap();
    let dom = String::from_utf8_lossy(&run.stdout);
    let log = String::from_utf8_lossy(&run.stderr);
    for id in ["list", "create", "get", "update", "remove", "health", "echo", "boom"] {
        assert!(dom.contains(&format!("id=\"op-{id}\"")), "no operation {id} in the page:\n{dom}");
    }
    assert!(dom.contains("Fetch one todo."), "the docstring's summary is missing");
    assert!(dom.contains("NewTodo"), "the models are missing");
    assert!(!log.contains("Uncaught"), "a script failed:\n{log}");
    server.stop(program);
}
