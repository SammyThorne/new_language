//! Every user-visible name of the language lives in this file.
//!
//! The language name is a working name. Renaming it means changing the constants here, the
//! binary name in `crates/lang_cli/Cargo.toml`, the extension of the `.wb` files under `tests/`,
//! and the docs. See "Renaming the language" in `docs/03-compiler-architecture.md`.
//!
//! No other Rust source file may spell the name or the file extension; the test at the bottom
//! of this file enforces that.

/// The language's display name, used in prose such as `--help` output.
pub const LANGUAGE_NAME: &str = "Weby";

/// The name of the compiler executable.
pub const CLI_NAME: &str = "weby";

/// Source file extension, without the leading dot.
pub const SOURCE_EXTENSION: &str = "wb";

/// Prefix of every environment variable the compiler or runtime reads.
pub const ENV_PREFIX: &str = "WEBY_";

/// Name of the directory, under the system temporary directory, used for build intermediates.
pub const CACHE_DIR_NAME: &str = "weby-cache";

/// The project manifest: the project's name and its dependencies.
pub const MANIFEST_FILE: &str = "weby.toml";

/// Where the exact commits of git dependencies are recorded.
pub const LOCK_FILE: &str = "weby.lock";

/// Where git dependencies are checked out, inside the project.
pub const DEPS_DIR: &str = ".weby/deps";

/// The full name of an environment variable, for example `env_var("RUNTIME")`.
pub fn env_var(suffix: &str) -> String {
    format!("{ENV_PREFIX}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                rust_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    /// The rename check from docs/03: outside this crate, Rust sources read the name and the
    /// extension from the constants above and never spell them out.
    #[test]
    fn name_is_only_spelled_in_this_crate() {
        let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let mut files = Vec::new();
        for entry in fs::read_dir(crates_dir).unwrap() {
            let dir = entry.unwrap().path();
            if dir.is_dir() && dir.file_name().unwrap() != "lang_names" {
                rust_files(&dir, &mut files);
            }
        }
        assert!(!files.is_empty(), "found no Rust sources under {}", crates_dir.display());

        let name = CLI_NAME.to_lowercase();
        let extension = format!(".{SOURCE_EXTENSION}\"");
        let offenders: Vec<String> = files
            .iter()
            .filter(|path| {
                let text = fs::read_to_string(path).unwrap().to_lowercase();
                text.contains(&name) || text.contains(&extension)
            })
            .map(|path| path.display().to_string())
            .collect();
        assert!(
            offenders.is_empty(),
            "these files spell the language name or extension instead of using lang_names: {offenders:?}"
        );
    }
}
