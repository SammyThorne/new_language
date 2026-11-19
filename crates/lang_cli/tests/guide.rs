//! The examples guide, `docs/examples.md`, checked against the real compiler so that it cannot
//! drift from the language.
//!
//! Each ```python block containing `def main():` is a complete program. If the block after it is
//! a ```text block, the program is run and its standard output must equal that block; otherwise
//! (servers, database and network clients) it is only checked. Blocks without `main` (fragments,
//! module files, test files) and blocks right after a `<!-- not compiled ... -->` line are skipped.

use lang_names::{CLI_NAME, SOURCE_EXTENSION};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn compiler() -> PathBuf {
    let variable = format!("CARGO_BIN_EXE_{CLI_NAME}");
    PathBuf::from(std::env::var_os(&variable).unwrap_or_else(|| panic!("cargo did not set {variable}")))
}

fn build_runtime() {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    command.args(["build", "--quiet", "-p", "lang_rt"]).current_dir(repo_root());
    if !cfg!(debug_assertions) {
        command.arg("--release");
    }
    assert!(command.status().unwrap().success(), "building the runtime library failed");
}

/// A fenced code block: its language tag, its text, the line it starts on, and whether the line
/// before it opts it out.
struct Block {
    lang: String,
    text: String,
    line: usize,
    skipped: bool,
}

fn blocks(markdown: &str) -> Vec<Block> {
    let mut found = Vec::new();
    let mut lines = markdown.lines().enumerate();
    let mut skip_next = false;
    while let Some((number, line)) = lines.next() {
        if line.starts_with("<!-- not compiled") {
            skip_next = true;
            continue;
        }
        let Some(lang) = line.strip_prefix("```") else {
            if !line.trim().is_empty() {
                skip_next = false;
            }
            continue;
        };
        let mut text = String::new();
        for (_, inner) in lines.by_ref() {
            if inner == "```" {
                break;
            }
            text.push_str(inner);
            text.push('\n');
        }
        found.push(Block { lang: lang.to_string(), text, line: number + 1, skipped: skip_next });
        skip_next = false;
    }
    found
}

#[test]
fn guide_programs() {
    build_runtime();
    let guide = repo_root().join("docs/examples.md");
    let blocks = blocks(&fs::read_to_string(&guide).unwrap());
    let dir = std::env::temp_dir().join(format!("{CLI_NAME}-guide-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let mut checked = 0;
    let mut failures = Vec::new();
    for (i, block) in blocks.iter().enumerate() {
        if block.lang != "python" || block.skipped || !block.text.contains("def main():") {
            continue;
        }
        checked += 1;
        let path = dir.join(format!("line{}.{SOURCE_EXTENSION}", block.line));
        fs::write(&path, &block.text).unwrap();
        let expected = blocks.get(i + 1).filter(|next| next.lang == "text").map(|next| next.text.as_str());
        let mut command = Command::new(compiler());
        command.arg(if expected.is_some() { "run" } else { "check" }).arg(&path).current_dir(&dir);
        let output = command.output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let fits = match expected {
            Some(expected) => output.status.success() && stdout == expected,
            None => output.status.success(),
        };
        if !fits {
            failures.push(format!(
                "docs/examples.md, the program at line {}: {}\n--- expected stdout\n{}--- actual stdout\n{stdout}--- stderr\n{stderr}",
                block.line,
                output.status,
                expected.unwrap_or("(only checked)\n")
            ));
        }
    }
    let _ = fs::remove_dir_all(&dir);
    assert!(checked >= 20, "found only {checked} programs in the guide; has its format changed?");
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}
