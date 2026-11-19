//! The project manifest and dependencies (docs/06, milestone 11).
//!
//! A project may have a manifest next to its main file, or in a directory above it:
//!
//! ```toml
//! [package]
//! name = "chat"
//!
//! [dependencies]
//! textutil = { git = "https://example.com/textutil.git", rev = "v1.2" }
//! shared = { path = "../shared" }
//! ```
//!
//! `import textutil.strings` then loads `strings` from the dependency's directory, and
//! `import textutil` its `lib` file. A dependency's own files import each other the same way,
//! with the dependency's name first. Git dependencies are cloned into the project's deps
//! directory the first time they are needed, and the commit used is written to the lock file;
//! later builds check out that commit, so a build is repeatable until the lock file is deleted
//! or `fetch --update` is run.
//!
//! The manifest is a small subset of TOML: sections, `key = "string"`, and inline tables of
//! strings.

use lang_names::{DEPS_DIR, LOCK_FILE, MANIFEST_FILE};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, PartialEq)]
pub enum Source {
    Path(PathBuf),
    Git { url: String, rev: Option<String> },
}

#[derive(Debug)]
pub struct Manifest {
    /// The directory holding the manifest.
    pub dir: PathBuf,
    pub name: Option<String>,
    pub dependencies: Vec<(String, Source)>,
}

/// The manifest that applies to `file`: in its directory or the nearest one above.
pub fn find(file: &Path) -> Option<PathBuf> {
    let dir = file.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let start = dir.canonicalize().ok()?;
    start.ancestors().map(|dir| dir.join(MANIFEST_FILE)).find(|path| path.is_file())
}

fn unquote(text: &str) -> Option<String> {
    let text = text.trim();
    let inner = text.strip_prefix('"')?.strip_suffix('"')?;
    Some(inner.replace("\\\"", "\"").replace("\\\\", "\\"))
}

pub fn parse(dir: &Path, text: &str) -> Result<Manifest, String> {
    let mut manifest = Manifest { dir: dir.to_path_buf(), name: None, dependencies: Vec::new() };
    let mut section = String::new();
    for (number, raw) in text.lines().enumerate() {
        let line = raw.split(" #").next().unwrap_or("").trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let at = |message: &str| format!("{MANIFEST_FILE}, line {}: {message}", number + 1);
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.trim().to_string();
            if !matches!(section.as_str(), "package" | "dependencies") {
                return Err(at(&format!("unknown section `[{section}]`; expected `[package]` or `[dependencies]`")));
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { return Err(at("expected `key = value`")) };
        let (key, value) = (key.trim().to_string(), value.trim());
        match section.as_str() {
            "package" => match key.as_str() {
                "name" => manifest.name = Some(unquote(value).ok_or_else(|| at("`name` must be a string"))?),
                "version" | "description" => {}
                _ => return Err(at(&format!("unknown key `{key}` in `[package]`"))),
            },
            "dependencies" => {
                let table = value
                    .strip_prefix('{')
                    .and_then(|v| v.strip_suffix('}'))
                    .ok_or_else(|| at("a dependency is `{ git = \"url\" }` or `{ path = \"dir\" }`"))?;
                let mut fields = BTreeMap::new();
                for pair in table.split(',').filter(|p| !p.trim().is_empty()) {
                    let (k, v) = pair.split_once('=').ok_or_else(|| at("expected `key = \"value\"` in the table"))?;
                    fields.insert(k.trim().to_string(), unquote(v).ok_or_else(|| at("values must be strings"))?);
                }
                let source = match (fields.remove("git"), fields.remove("path")) {
                    (Some(url), None) => Source::Git { url, rev: fields.remove("rev") },
                    (None, Some(path)) => Source::Path(dir.join(path)),
                    _ => return Err(at(&format!("dependency `{key}` needs exactly one of `git` and `path`"))),
                };
                if let Some(extra) = fields.keys().next() {
                    return Err(at(&format!("unknown key `{extra}` for dependency `{key}`")));
                }
                manifest.dependencies.push((key, source));
            }
            _ => return Err(at("keys must be inside `[package]` or `[dependencies]`")),
        }
    }
    Ok(manifest)
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|error| format!("could not run `git`: {error}"))?;
    if !output.status.success() {
        return Err(format!("`git {}` failed: {}", args.join(" "), String::from_utf8_lossy(&output.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn read_lock(dir: &Path) -> BTreeMap<String, String> {
    let text = fs::read_to_string(dir.join(LOCK_FILE)).unwrap_or_default();
    text.lines()
        .filter_map(|line| line.split_once('=').and_then(|(k, v)| Some((k.trim().to_string(), unquote(v)?))))
        .collect()
}

fn write_lock(dir: &Path, lock: &BTreeMap<String, String>) -> Result<(), String> {
    let mut text = String::from("# Commits of git dependencies; written by the compiler.\n");
    for (name, commit) in lock {
        text.push_str(&format!("{name} = \"{commit}\"\n"));
    }
    fs::write(dir.join(LOCK_FILE), text).map_err(|e| format!("cannot write {LOCK_FILE}: {e}"))
}

/// The directory of each dependency. With `fetch`, git dependencies that are missing (or not
/// at their locked commit) are cloned or checked out, and the lock file updated; without it,
/// only what is already there is used (for the language server). With `update`, the lock file
/// is ignored and each git dependency moved to its `rev` (or newest commit).
pub fn resolve(manifest: &Manifest, fetch: bool, update: bool) -> Result<BTreeMap<String, PathBuf>, String> {
    let mut roots = BTreeMap::new();
    let mut lock = if update { BTreeMap::new() } else { read_lock(&manifest.dir) };
    let mut lock_changed = false;
    for (name, source) in &manifest.dependencies {
        match source {
            Source::Path(path) => {
                if !path.is_dir() {
                    return Err(format!("dependency `{name}`: `{}` is not a directory", path.display()));
                }
                roots.insert(name.clone(), path.clone());
            }
            Source::Git { url, rev } => {
                let dir = manifest.dir.join(DEPS_DIR).join(name);
                if !fetch {
                    if dir.is_dir() {
                        roots.insert(name.clone(), dir);
                    }
                    continue;
                }
                if !dir.join(".git").is_dir() {
                    eprintln!("fetching `{name}` from {url}");
                    fs::create_dir_all(dir.parent().expect("a parent")).map_err(|e| e.to_string())?;
                    git(&manifest.dir, &["clone", "--quiet", url, &dir.display().to_string()])?;
                } else if update {
                    git(&dir, &["fetch", "--quiet", "--tags", "origin"])?;
                }
                let wanted = match (lock.get(name), rev) {
                    (Some(commit), _) => Some(commit.clone()),
                    (None, Some(rev)) => Some(rev.clone()),
                    (None, None) if update => Some("origin/HEAD".to_string()),
                    (None, None) => None,
                };
                if let Some(wanted) = wanted {
                    let current = git(&dir, &["rev-parse", "HEAD"])?;
                    let target = git(&dir, &["rev-parse", &format!("{wanted}^{{commit}}")])
                        .or_else(|_| {
                            git(&dir, &["fetch", "--quiet", "--tags", "origin"])?;
                            git(&dir, &["rev-parse", &format!("{wanted}^{{commit}}")])
                        })
                        .map_err(|_| {
                            format!("dependency `{name}`: `{wanted}` is not a commit, tag or branch of {url}")
                        })?;
                    if current != target {
                        git(&dir, &["-c", "advice.detachedHead=false", "checkout", "--quiet", &target])?;
                    }
                }
                let commit = git(&dir, &["rev-parse", "HEAD"])?;
                if lock.get(name) != Some(&commit) {
                    lock.insert(name.clone(), commit);
                    lock_changed = true;
                }
                roots.insert(name.clone(), dir);
            }
        }
    }
    if fetch && lock_changed {
        lock.retain(|name, _| manifest.dependencies.iter().any(|(n, _)| n == name));
        write_lock(&manifest.dir, &lock)?;
    }
    Ok(roots)
}

/// Find, read and resolve the manifest for `file`, if there is one.
pub fn dependencies_for(file: &Path, fetch: bool, update: bool) -> Result<BTreeMap<String, PathBuf>, String> {
    let Some(path) = find(file) else { return Ok(BTreeMap::new()) };
    let text = fs::read_to_string(&path).map_err(|e| format!("cannot read `{}`: {e}", path.display()))?;
    let manifest = parse(path.parent().expect("a directory"), &text)?;
    resolve(&manifest, fetch, update)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_documented_form() {
        let text = "[package]\nname = \"chat\"\n\n[dependencies]\ntextutil = { git = \"https://example.com/t.git\", rev = \"v1.2\" }\nshared = { path = \"../shared\" }  # local\n";
        let manifest = parse(Path::new("/p"), text).unwrap();
        assert_eq!(manifest.name.as_deref(), Some("chat"));
        assert_eq!(
            manifest.dependencies,
            vec![
                (
                    "textutil".to_string(),
                    Source::Git { url: "https://example.com/t.git".into(), rev: Some("v1.2".into()) }
                ),
                ("shared".to_string(), Source::Path(PathBuf::from("/p/../shared"))),
            ]
        );
        assert!(parse(Path::new("/p"), "[deps]\n").unwrap_err().contains("unknown section"));
        assert!(parse(Path::new("/p"), "[dependencies]\nx = { url = \"a\" }\n").unwrap_err().contains("exactly one"));
    }
}
