//! Migrations (docs/14, milestone 18) outside the in-process tests: two programs starting at the
//! same time, on SQLite and (with `<PREFIX>TEST_DATABASE_URL`) on PostgreSQL, where one applies
//! and the other waits for the advisory lock; the PostgreSQL scenarios; and the command line.

mod common;

use common::{build_runtime, compiler, repo_root};
use lang_names::{SOURCE_EXTENSION, env_var};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The PostgreSQL tests share one database and the `schema_migrations` table in it.
static DATABASE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("migrate-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn build(name: &str, dir: &Path) -> PathBuf {
    let source = repo_root().join(format!("tests/db/{name}.{SOURCE_EXTENSION}"));
    let exe = dir.join(name);
    let output = Command::new(compiler())
        .arg("build")
        .arg(&source)
        .arg("-o")
        .arg(&exe)
        .current_dir(repo_root())
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    exe
}

/// Start two copies of the race program together and return what each printed.
fn race(exe: &Path, database: &str, migrations: &Path) -> Vec<String> {
    let children: Vec<_> = (0..2)
        .map(|_| {
            Command::new(exe)
                .arg(database)
                .arg(migrations)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    children
        .into_iter()
        .map(|child| {
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        })
        .collect()
}

/// Exactly one of the runs applied the files, and the other found nothing to do.
fn assert_one_applied(mut outputs: Vec<String>, all: &str) {
    outputs.sort();
    assert_eq!(
        outputs,
        vec![String::new(), all.to_string()],
        "one process applies, the other waits and applies nothing"
    );
}

#[test]
fn two_processes_share_a_sqlite_file() {
    build_runtime();
    let dir = scratch("sqlite");
    let migrations = dir.join("migrations");
    std::fs::create_dir_all(&migrations).unwrap();
    // The first file is slow to run, so the second process arrives while it holds the write lock.
    std::fs::write(
        migrations.join("0001_big.sql"),
        "create table big (id integer primary key, n integer);\n\
         with recursive c(x) as (select 1 union all select x + 1 from c where x < 300000) \
         insert into big (n) select x from c;",
    )
    .unwrap();
    std::fs::write(migrations.join("0002_after.sql"), "create table after_it (id integer primary key);").unwrap();
    let exe = build("migrations_race", &dir);
    let file = dir.join("race.db");
    let outputs = race(&exe, file.to_str().unwrap(), &migrations);
    assert_one_applied(outputs, "0001_big.sql 0002_after.sql");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn two_processes_share_a_postgres_database() {
    let Ok(url) = std::env::var(env_var("TEST_DATABASE_URL")) else {
        eprintln!("skipped: set {} to run the PostgreSQL test", env_var("TEST_DATABASE_URL"));
        return;
    };
    let _exclusive = DATABASE.lock().unwrap_or_else(|e| e.into_inner());
    build_runtime();
    let dir = scratch("postgres");
    let migrations = dir.join("migrations");
    std::fs::create_dir_all(&migrations).unwrap();
    // A pause inside the first migration, so the other process reaches the lock while it is held.
    std::fs::write(
        migrations.join("0001_slow.sql"),
        "select pg_sleep(1.5);\ncreate table mig_race (id integer primary key);",
    )
    .unwrap();
    std::fs::write(migrations.join("0002_after.sql"), "insert into mig_race values (1);").unwrap();
    let exe = build("migrations_race", &dir);
    reset(&exe, &url);
    let outputs = race(&exe, &url, &migrations);
    assert_one_applied(outputs, "0001_slow.sql 0002_after.sql");
    reset(&exe, &url);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Drop the tables a race leaves behind.
fn reset(exe: &Path, url: &str) {
    let output = Command::new(exe).arg(url).arg("reset").output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
}

#[test]
fn postgres_migrations() {
    let Ok(url) = std::env::var(env_var("TEST_DATABASE_URL")) else {
        eprintln!("skipped: set {} to run the PostgreSQL test", env_var("TEST_DATABASE_URL"));
        return;
    };
    let _exclusive = DATABASE.lock().unwrap_or_else(|e| e.into_inner());
    build_runtime();
    let root = repo_root();
    let program = root.join(format!("tests/db/migrations_postgres.{SOURCE_EXTENSION}"));
    let source = std::fs::read_to_string(&program).unwrap();
    let expected: String =
        source.lines().filter_map(|l| l.strip_prefix("# expect: ")).map(|l| format!("{l}\n")).collect();
    let output = Command::new(compiler()).arg("run").arg(&program).arg(&url).current_dir(&root).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout == expected,
        "{}\n--- expected\n{expected}--- actual\n{stdout}--- stderr\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn cli(dir: &Path, args: &[&str]) -> std::process::Output {
    build_runtime();
    Command::new(compiler()).arg("migrate").args(args).current_dir(dir).env_remove("DATABASE_URL").output().unwrap()
}

#[test]
fn command_line_creates_and_reports() {
    let dir = scratch("cli");
    let made = cli(&dir, &["new", "Add users"]);
    assert!(made.status.success());
    assert_eq!(String::from_utf8_lossy(&made.stdout).trim(), "migrations/0001_add_users.sql");
    let made = cli(&dir, &["new", "add  tags!"]);
    assert_eq!(String::from_utf8_lossy(&made.stdout).trim(), "migrations/0002_add_tags.sql");
    assert_eq!(std::fs::read_to_string(dir.join("migrations/0002_add_tags.sql")).unwrap(), "-- add  tags!\n");
    // A higher number in the directory sets the next one.
    std::fs::write(dir.join("migrations/0010_big.sql"), "select 1;").unwrap();
    let made = cli(&dir, &["new", "later"]);
    assert_eq!(String::from_utf8_lossy(&made.stdout).trim(), "migrations/0011_later.sql");
    let refused = cli(&dir, &["new", "!!!"]);
    assert!(!refused.status.success());

    std::fs::write(dir.join("migrations/0001_add_users.sql"), "create table users (id integer);").unwrap();
    std::fs::remove_file(dir.join("migrations/0010_big.sql")).unwrap();
    std::fs::remove_file(dir.join("migrations/0011_later.sql")).unwrap();
    let status = cli(&dir, &["status", "--database", "app.db"]);
    assert!(status.status.success(), "{}", String::from_utf8_lossy(&status.stderr));
    assert_eq!(
        String::from_utf8_lossy(&status.stdout),
        "pending  0001_add_users.sql\npending  0002_add_tags.sql\n0 applied, 2 pending\n"
    );
    let missing = cli(&dir, &["status"]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("no database"));
    let _ = std::fs::remove_dir_all(&dir);
}
