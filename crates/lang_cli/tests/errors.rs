//! The error reference, `docs/errors.md`, has a section for every diagnostic code (docs/11,
//! milestone 5), and no section for a code that does not exist, so that it cannot fall behind.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

fn repo_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

/// The codes declared in `crates/lang_diag/src/codes.rs`: the quoted `E0000` and `W0000` values.
fn declared_codes() -> BTreeSet<String> {
    let text = fs::read_to_string(repo_root().join("crates/lang_diag/src/codes.rs")).unwrap();
    text.lines()
        .filter(|line| line.starts_with("pub const "))
        .filter_map(|line| line.split('"').nth(1))
        .map(String::from)
        .collect()
}

/// The codes with a section, `### E0300: ...`, in the reference.
fn documented_codes() -> BTreeSet<String> {
    let text = fs::read_to_string(repo_root().join("docs/errors.md")).unwrap();
    text.lines()
        .filter_map(|line| line.strip_prefix("### "))
        .filter_map(|heading| heading.split(':').next())
        .map(String::from)
        .collect()
}

#[test]
fn every_code_has_a_section() {
    let declared = declared_codes();
    let documented = documented_codes();
    assert!(declared.len() > 50, "found only {} codes", declared.len());
    let missing: Vec<_> = declared.difference(&documented).collect();
    assert!(missing.is_empty(), "docs/errors.md has no section for {missing:?}");
    let unknown: Vec<_> = documented.difference(&declared).collect();
    assert!(unknown.is_empty(), "docs/errors.md has sections for codes that do not exist: {unknown:?}");
}

/// Every code is produced by some test program in `tests/ui`, so that each section's example
/// is a real message.
#[test]
fn every_code_has_an_example() {
    let mut seen = BTreeSet::new();
    for entry in fs::read_dir(repo_root().join("tests/ui")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "stderr") {
            for line in fs::read_to_string(&path).unwrap().lines() {
                if let Some(rest) = line.strip_prefix("error[").or_else(|| line.strip_prefix("warning[")) {
                    seen.insert(rest.split(']').next().unwrap().to_string());
                }
            }
        }
    }
    let missing: Vec<_> = declared_codes().difference(&seen).cloned().collect();
    assert!(missing.is_empty(), "no test in tests/ui gives {missing:?}");
}
