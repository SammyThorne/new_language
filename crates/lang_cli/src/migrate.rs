//! `migrate new` and `migrate status`: the command-line side of the `migrate` standard library
//! module (docs/14, section 4.15). Creating a file needs no database. Reporting does, and the
//! database drivers are part of the runtime, so `status` builds and runs a small program that
//! calls the module, as `run` would.

use super::{EXIT_COMPILE_ERROR, Failure, TempDir, run};
use lang_codegen::OptLevel;
use lang_names::SOURCE_EXTENSION;
use std::path::Path;
use std::process::ExitCode;
use std::{env, fs};

/// The directory migrations live in unless `--dir` says otherwise.
pub const DEFAULT_DIR: &str = "migrations";

/// The program `status` runs. Its first argument is the database (empty for the `DATABASE_URL`
/// environment variable) and its second the directory.
const STATUS_PROGRAM: &str = r#"import io
import migrate
import os
import postgres
import sqlite

def main():
    args = os.args()
    url = args[1]
    if url == "":
        match os.getenv("DATABASE_URL"):
            case Some(value):
                url = value
            case None:
                io.eprint("error: no database: give --database or set DATABASE_URL")
                os.exit(1)
    if url.starts_with("postgres://") or url.starts_with("postgresql://"):
        match postgres.connect(url):
            case Ok(db):
                os.exit(migrate.run_status(db, args[2]))
            case Err(e):
                io.eprint(f"error: {e.message()}")
                os.exit(1)
    path = url
    if path.starts_with("sqlite:"):
        path = path[7:]
    match sqlite.open(path):
        case Ok(file):
            os.exit(migrate.run_status(file, args[2]))
        case Err(problem):
            io.eprint(f"error: {problem.message()}")
            os.exit(1)
"#;

fn fail(message: String) -> Failure {
    eprintln!("error: {message}");
    Failure(EXIT_COMPILE_ERROR)
}

/// A file-name form of a description: lower case, runs of other characters as one `_`.
fn slug(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('_') {
            out.push('_');
        }
    }
    out.trim_end_matches('_').to_string()
}

/// The number of a migration file, `3` for `0003_add_tags.sql`.
fn version_of(file: &str) -> Option<u64> {
    let stem = file.strip_suffix(".sql")?;
    let (digits, rest) = stem.split_once('_')?;
    if rest.is_empty() || digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// `migrate new <name>`: the next file after the highest number in the directory, which is
/// created if it does not exist.
pub fn new_file(name: &str, dir: &Path) -> Result<ExitCode, Failure> {
    let slug = slug(name);
    if slug.is_empty() {
        return Err(fail("the migration needs a name made of letters or digits".to_string()));
    }
    fs::create_dir_all(dir).map_err(|error| fail(format!("cannot create `{}`: {error}", dir.display())))?;
    let entries = fs::read_dir(dir).map_err(|error| fail(format!("cannot read `{}`: {error}", dir.display())))?;
    let newest = entries
        .filter_map(Result::ok)
        .filter_map(|entry| version_of(&entry.file_name().to_string_lossy()))
        .max()
        .unwrap_or(0);
    let file = dir.join(format!("{:04}_{slug}.sql", newest + 1));
    let text = format!("-- {}\n", name.trim());
    fs::write(&file, text).map_err(|error| fail(format!("cannot write `{}`: {error}", file.display())))?;
    println!("{}", file.display());
    Ok(ExitCode::SUCCESS)
}

/// `migrate status`: what has run and what is pending.
pub fn status(dir: &Path, database: Option<&str>) -> Result<ExitCode, Failure> {
    let temp = TempDir::new()?;
    let program = temp.path().join(format!("status.{SOURCE_EXTENSION}"));
    fs::write(&program, STATUS_PROGRAM)
        .map_err(|error| fail(format!("cannot write `{}`: {error}", program.display())))?;
    let directory = env::current_dir().map(|cwd| cwd.join(dir)).unwrap_or_else(|_| dir.to_path_buf());
    let args = [database.unwrap_or("").to_string(), directory.to_string_lossy().into_owned()];
    run(&program, OptLevel::Debug, &args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_become_file_names() {
        assert_eq!(slug("Add tags"), "add_tags");
        assert_eq!(slug("  add -- tags!! "), "add_tags");
        assert_eq!(slug("!!!"), "");
    }

    #[test]
    fn versions_are_read_from_file_names() {
        assert_eq!(version_of("0003_add_tags.sql"), Some(3));
        assert_eq!(version_of("12_x.sql"), Some(12));
        assert_eq!(version_of("notes.sql"), None);
        assert_eq!(version_of("0001_.sql"), None);
        assert_eq!(version_of("0001_x.txt"), None);
    }
}
