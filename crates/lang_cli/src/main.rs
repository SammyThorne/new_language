//! The compiler's command-line driver: runs the pipeline in docs/03-compiler-architecture.md
//! and links the result with the runtime library.

mod fmt;
mod loader;
mod lsp;
mod manifest;

use clap::{Parser, Subcommand, ValueEnum};
use lang_codegen::OptLevel;
use lang_diag::{Diagnostics, SourceMap, render};
use lang_names::{CACHE_DIR_NAME, CLI_NAME, LANGUAGE_NAME, SOURCE_EXTENSION, env_var};
use lang_sema::hir;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::atomic::{AtomicU32, Ordering};
use std::{env, fs};

/// File name of the runtime library, built from `crates/lang_rt`.
const RUNTIME_LIBRARY: &str = "liblang_rt.a";

/// System libraries the runtime needs, as reported by
/// `cargo rustc -p lang_rt -- --print native-static-libs` on Linux.
/// The garbage collector is the system's Boehm GC, `libgc.so.1` (docs/04-runtime-and-memory.md).
const SYSTEM_LIBRARIES: &[&str] = &["-l:libgc.so.1", "-lgcc_s", "-lutil", "-lrt", "-lpthread", "-lm", "-ldl", "-lc"];

/// Exit status for a compile error in the user's program.
const EXIT_COMPILE_ERROR: u8 = 1;
/// Exit status for a bug in the compiler itself.
const EXIT_INTERNAL_ERROR: u8 = 101;

#[derive(Parser)]
#[command(
    name = CLI_NAME,
    version,
    about = format!("Compiler for the {LANGUAGE_NAME} programming language"),
    after_help = format!(
        "With no command, `build` is assumed: `{CLI_NAME} app.{SOURCE_EXTENSION} -o app` is `{CLI_NAME} build app.{SOURCE_EXTENSION} -o app`."
    )
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Compile a source file into an executable
    Build {
        /// The source file containing `main`
        file: PathBuf,
        /// Where to write the output (default: the source file's name, in the current directory)
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Optimise the generated code
        #[arg(long)]
        release: bool,
        /// Write an intermediate form instead of an executable
        #[arg(long, value_enum)]
        emit: Option<Emit>,
    },
    /// Compile a source file and run it
    Run {
        /// The source file containing `main`
        file: PathBuf,
        /// Optimise the generated code
        #[arg(long)]
        release: bool,
        /// Arguments passed to the program
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Check a source file for errors without generating code
    Check {
        /// The source file to check
        file: PathBuf,
    },
    /// Run the `@test` functions in source files
    Test {
        /// Files or directories (searched recursively); the current directory if none
        paths: Vec<PathBuf>,
        /// Only run tests whose name contains this text
        #[arg(long)]
        filter: Option<String>,
        /// Optimise the generated code
        #[arg(long)]
        release: bool,
    },
    /// Fetch the project's git dependencies and record their commits
    Fetch {
        /// A file of the project (the manifest is found from it); the current directory if none
        file: Option<PathBuf>,
        /// Move each dependency to its `rev` (or newest commit) and rewrite the lock file
        #[arg(long)]
        update: bool,
    },
    /// Run the language server, speaking the Language Server Protocol on standard input and output
    Lsp,
    /// Rewrite source files in the standard layout
    Fmt {
        /// Files or directories (searched recursively); the current directory if none
        paths: Vec<PathBuf>,
        /// Change nothing; list the files that are not formatted and fail if there are any
        #[arg(long)]
        check: bool,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Emit {
    /// The token stream, including layout tokens
    Tokens,
    /// The syntax tree
    Ast,
    /// The typed intermediate representation
    Hir,
    /// LLVM IR, as text
    LlvmIr,
    /// A native object file
    Obj,
}

/// A failed command. The message has already been printed.
struct Failure(u8);

fn main() -> ExitCode {
    let cli = Cli::parse_from(with_default_command(env::args_os().collect()));
    let result = match cli.command {
        Cmd::Build { file, output, release, emit } => build(&file, output, opt_level(release), emit),
        Cmd::Run { file, release, args } => run(&file, opt_level(release), &args),
        Cmd::Check { file } => frontend(&file, None).map(|_| ExitCode::SUCCESS),
        Cmd::Fmt { paths, check } => format_files(&paths, check),
        Cmd::Fetch { file, update } => {
            let file = file.unwrap_or_else(|| PathBuf::from(".").join("main"));
            match manifest::dependencies_for(&file, true, update) {
                Ok(dependencies) => {
                    for (name, dir) in dependencies {
                        println!("{name}: {}", dir.display());
                    }
                    Ok(ExitCode::SUCCESS)
                }
                Err(message) => {
                    eprintln!("error: {message}");
                    Err(Failure(EXIT_COMPILE_ERROR))
                }
            }
        }
        Cmd::Lsp => {
            let status = lsp::serve(&mut std::io::stdin().lock(), &mut std::io::stdout().lock());
            Ok(ExitCode::from(status))
        }
        Cmd::Test { paths, filter, release } => run_tests(&paths, filter.as_deref(), opt_level(release)),
    };
    result.unwrap_or_else(|Failure(code)| ExitCode::from(code))
}

/// `build` is the default command: `<cli> app.<ext> -o app` means `<cli> build app.<ext> -o app`. It is
/// inserted when the first argument is a source file, or a flag that is not `--help` or
/// `--version` (as in `<cli> --release app.<ext>`). Anything else, including a mistyped command,
/// is left for clap to report.
fn with_default_command(mut args: Vec<std::ffi::OsString>) -> Vec<std::ffi::OsString> {
    use clap::CommandFactory;
    let Some(first) = args.get(1).and_then(|arg| arg.to_str()) else { return args };
    let command = Cli::command();
    let is_command = command.get_subcommands().any(|c| c.get_name() == first) || first == "help";
    let is_source = Path::new(first).extension().is_some_and(|ext| ext == SOURCE_EXTENSION);
    let is_build_flag = first.starts_with('-') && !matches!(first, "-h" | "--help" | "-V" | "--version");
    if !is_command && (is_source || is_build_flag) {
        args.insert(1, "build".into());
    }
    args
}

fn opt_level(release: bool) -> OptLevel {
    if release { OptLevel::Release } else { OptLevel::Debug }
}

fn internal_error(message: String) -> Failure {
    eprintln!("internal compiler error: {message}");
    Failure(EXIT_INTERNAL_ERROR)
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 { format!("{n} {word}") } else { format!("{n} {word}s") }
}

/// Lex, parse and type-check a program: the given file and every module it imports. Prints
/// diagnostics. Returns the program, or `None` when `emit` asked for an earlier stage, which has
/// then been printed.
fn frontend(file: &Path, emit: Option<Emit>) -> Result<Option<(hir::Program, SourceMap)>, Failure> {
    frontend_with(file, emit, None)
}

/// [`frontend`], optionally for testing: with `test`, the generated test runner is appended to
/// the file's text and the program starts there instead of at `main`.
fn frontend_with(
    file: &Path,
    emit: Option<Emit>,
    test: Option<&str>,
) -> Result<Option<(hir::Program, SourceMap)>, Failure> {
    if file.extension().is_none_or(|ext| ext != SOURCE_EXTENSION) {
        eprintln!(
            "error: `{}` is not a {LANGUAGE_NAME} source file; expected a `.{SOURCE_EXTENSION}` file",
            file.display()
        );
        return Err(Failure(EXIT_COMPILE_ERROR));
    }
    let text = fs::read_to_string(file).map_err(|error| {
        eprintln!("error: cannot read `{}`: {error}", file.display());
        Failure(EXIT_COMPILE_ERROR)
    })?;
    let text = match test {
        Some(runner) => format!("{text}\n{runner}"),
        None => text,
    };
    let display = file.display().to_string();
    let mut sources = SourceMap::default();
    let mut diags = Diagnostics::default();
    let file_id = sources.add(display.clone(), text);

    let tokens = lang_syntax::lex(file_id, &sources.get(file_id).text, &mut diags);
    if emit == Some(Emit::Tokens) {
        let source = sources.get(file_id);
        for token in &tokens {
            let (line, col) = source.line_col(token.span.lo as usize);
            println!("{line}:{col} {}", token.kind);
        }
        return finish_early(&display, &sources, &diags);
    }

    let module = lang_syntax::parse(&tokens, &mut diags);
    if emit == Some(Emit::Ast) {
        println!("{module:#?}");
        return finish_early(&display, &sources, &diags);
    }

    let module_name = file.file_stem().map_or("main".into(), |stem| stem.to_string_lossy().into_owned());
    let root = file.parent().map(Path::to_path_buf).unwrap_or_default();
    let dependencies = manifest::dependencies_for(file, true, false).map_err(|message| {
        eprintln!("error: {message}");
        Failure(EXIT_COMPILE_ERROR)
    })?;
    let modules = loader::load(&root, &dependencies, module_name, module, &mut sources, &mut diags);

    // Type errors in a file that did not parse are mostly noise (a function whose header failed
    // to parse is "unknown" at every call), so checking waits for a clean parse.
    let program = if diags.has_errors() {
        None
    } else {
        let inputs: Vec<lang_sema::ModuleInput> = modules
            .iter()
            .map(|m| lang_sema::ModuleInput { name: m.name.clone(), ast: &m.ast, stdlib: m.stdlib })
            .collect();
        lang_sema::check_program(&inputs, &mut sources, &mut diags, if test.is_some() { TEST_ENTRY } else { "main" })
    };
    report(&display, &sources, &diags)?;
    let program = program.ok_or_else(|| internal_error("type checking failed without reporting an error".into()))?;
    if emit == Some(Emit::Hir) {
        println!("{program:#?}");
        return Ok(None);
    }
    Ok(Some((program, sources)))
}

fn finish_early(
    display: &str,
    sources: &SourceMap,
    diags: &Diagnostics,
) -> Result<Option<(hir::Program, SourceMap)>, Failure> {
    report(display, sources, diags)?;
    Ok(None)
}

/// Print every diagnostic, then fail if any of them was an error.
fn report(display: &str, sources: &SourceMap, diags: &Diagnostics) -> Result<(), Failure> {
    for diagnostic in diags.sorted() {
        eprintln!("{}", render(diagnostic, sources));
    }
    if diags.has_errors() {
        eprintln!("error: could not compile `{display}` due to {}", plural(diags.error_count(), "error"));
        return Err(Failure(EXIT_COMPILE_ERROR));
    }
    Ok(())
}

fn build(file: &Path, output: Option<PathBuf>, opt: OptLevel, emit: Option<Emit>) -> Result<ExitCode, Failure> {
    let Some((program, sources)) = frontend(file, emit)? else { return Ok(ExitCode::SUCCESS) };
    let stem = PathBuf::from(&program.module);
    match emit {
        Some(Emit::LlvmIr) => {
            let ir = lang_codegen::emit_llvm_ir(&program, &sources, opt).map_err(internal_error)?;
            match output {
                Some(path) => write_file(&path, &ir)?,
                None => print!("{ir}"),
            }
        }
        Some(Emit::Obj) => {
            let path = output.unwrap_or_else(|| stem.with_extension("o"));
            lang_codegen::emit_object(&program, &sources, opt, &path).map_err(internal_error)?;
        }
        Some(Emit::Tokens | Emit::Ast | Emit::Hir) => unreachable!("handled by the frontend"),
        None => link_executable(&program, &sources, opt, &output.unwrap_or(stem))?,
    }
    Ok(ExitCode::SUCCESS)
}

fn run(file: &Path, opt: OptLevel, args: &[String]) -> Result<ExitCode, Failure> {
    let (program, sources) = frontend(file, None)?.expect("no emit stage was requested");
    let temp = TempDir::new()?;
    let executable = temp.path().join(&program.module);
    link_executable(&program, &sources, opt, &executable)?;
    let status = Command::new(&executable).args(args).status().map_err(|error| {
        eprintln!("error: could not start `{}`: {error}", executable.display());
        Failure(EXIT_COMPILE_ERROR)
    })?;
    drop(temp);

    // Pass the program's exit status through, as a shell would report it.
    use std::os::unix::process::ExitStatusExt;
    let code = status.code().or_else(|| status.signal().map(|signal| 128 + signal)).unwrap_or(1);
    Ok(ExitCode::from(code as u8))
}

/// The function the generated test runner starts at.
const TEST_ENTRY: &str = "__test_main";

/// The `@test` functions of a file, with whether each returns a `Result`; `None` if the file
/// does not parse (the build reports why).
fn find_tests(file: &Path) -> Option<Vec<(String, bool)>> {
    let text = fs::read_to_string(file).ok()?;
    let mut sources = SourceMap::default();
    let mut diags = Diagnostics::default();
    let id = sources.add(file.display().to_string(), text);
    let tokens = lang_syntax::lex(id, &sources.get(id).text, &mut diags);
    let module = lang_syntax::parse(&tokens, &mut diags);
    Some(
        module
            .items
            .iter()
            .filter_map(|item| match item {
                lang_syntax::ast::Item::Function(f) if f.annotations.iter().any(|a| a.name.name == "test") => {
                    let returns = matches!(&f.ret, Some(t) if !matches!(t.kind, lang_syntax::ast::TypeExprKind::None));
                    Some((f.name.name.clone(), returns))
                }
                _ => None,
            })
            .collect(),
    )
}

/// Source for the test runner: run each test in its own task, so that a failing `assert` (a
/// panic) ends only that test, and print a line per test for [`run_tests`] to read.
fn test_runner(tests: &[(String, bool)]) -> String {
    let mut wrappers = String::new();
    let mut text = format!("def {TEST_ENTRY}():\n");
    for (index, (name, returns_result)) in tests.iter().enumerate() {
        // A test that returns `Err` fails: a wrapper turns that into a panic.
        let target = if *returns_result {
            wrappers.push_str(&format!(
                "def __test_{index}():\n    match {name}():\n        case Ok(_):\n            pass\n        case Err(__error):\n            panic(f\"the test returned an error: {{__error.message()}}\")\n\n"
            ));
            format!("__test_{index}")
        } else {
            name.clone()
        };
        text.push_str(&format!(
            "    if (spawn {target}()).__panicked():\n        print(\"test {name} ... FAILED\")\n    else:\n        print(\"test {name} ... ok\")\n"
        ));
    }
    if tests.is_empty() {
        text.push_str("    pass\n");
    }
    wrappers + &text
}

/// `test`: build each file that has `@test` functions with a generated runner, run it, and
/// report. Fails if a file does not compile or any test fails.
fn run_tests(paths: &[PathBuf], filter: Option<&str>, opt: OptLevel) -> Result<ExitCode, Failure> {
    let (mut passed, mut failed, mut broken) = (0, 0, 0);
    for file in source_files(paths) {
        let Some(tests) = find_tests(&file) else { continue };
        let tests: Vec<(String, bool)> =
            tests.into_iter().filter(|(name, _)| filter.is_none_or(|f| name.contains(f))).collect();
        if tests.is_empty() {
            continue;
        }
        println!("running {} in {}", plural(tests.len(), "test"), file.display());
        let runner = test_runner(&tests);
        let Ok(Some((program, sources))) = frontend_with(&file, None, Some(&runner)) else {
            broken += 1;
            continue;
        };
        let temp = TempDir::new()?;
        let executable = temp.path().join(&program.module);
        link_executable(&program, &sources, opt, &executable)?;
        let output = Command::new(&executable).output().map_err(|error| {
            eprintln!("error: could not start `{}`: {error}", executable.display());
            Failure(EXIT_COMPILE_ERROR)
        })?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let mut file_passed = 0;
        let mut file_failed = 0;
        for line in stdout.lines() {
            if line.starts_with("test ") && line.ends_with(" ... ok") {
                file_passed += 1;
            } else if line.starts_with("test ") && line.ends_with(" ... FAILED") {
                file_failed += 1;
            }
            println!("{line}");
        }
        if !output.status.success() || file_failed > 0 {
            eprint!("{stderr}");
        }
        if !output.status.success() {
            eprintln!("error: the tests in `{}` stopped early: {}", file.display(), output.status);
            broken += 1;
        }
        passed += file_passed;
        failed += file_failed;
    }
    let ok = failed == 0 && broken == 0;
    println!(
        "\ntest result: {}. {passed} passed; {failed} failed{}",
        if ok { "ok" } else { "FAILED" },
        if broken > 0 { format!("; {} could not run", plural(broken, "file")) } else { String::new() }
    );
    if ok { Ok(ExitCode::SUCCESS) } else { Err(Failure(EXIT_COMPILE_ERROR)) }
}

/// Source files under `paths` (files as given, directories searched), skipping hidden
/// directories and build output.
fn source_files(paths: &[PathBuf]) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else { return };
        let mut entries: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        entries.sort();
        for path in entries {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if path.is_dir() {
                if !name.starts_with('.') && name != "target" {
                    walk(&path, out);
                }
            } else if path.extension().is_some_and(|e| e == SOURCE_EXTENSION) {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    let defaults = [PathBuf::from(".")];
    for path in if paths.is_empty() { &defaults[..] } else { paths } {
        if path.is_dir() {
            walk(path, &mut files);
        } else {
            files.push(path.clone());
        }
    }
    files
}

/// `fmt`: format files in place, or with `check`, only report the ones that would change.
fn format_files(paths: &[PathBuf], check: bool) -> Result<ExitCode, Failure> {
    let mut unformatted = 0;
    let mut failed = false;
    for path in source_files(paths) {
        let text = fs::read_to_string(&path).map_err(|error| {
            eprintln!("error: cannot read `{}`: {error}", path.display());
            Failure(EXIT_COMPILE_ERROR)
        })?;
        let Some(before) = fmt::token_fingerprint(&text) else {
            eprintln!("error: `{}` has syntax errors; run `{CLI_NAME} check` on it", path.display());
            failed = true;
            continue;
        };
        let formatted = match fmt::format(&text) {
            Some(formatted) if fmt::token_fingerprint(&formatted).as_ref() == Some(&before) => formatted,
            _ => {
                eprintln!(
                    "internal error: formatting `{}` would change its meaning; it was left alone",
                    path.display()
                );
                failed = true;
                continue;
            }
        };
        if formatted == text {
            continue;
        }
        unformatted += 1;
        if check {
            println!("{}", path.display());
        } else {
            write_file(&path, &formatted)?;
        }
    }
    if failed || (check && unformatted > 0) {
        return Err(Failure(EXIT_COMPILE_ERROR));
    }
    Ok(ExitCode::SUCCESS)
}

fn write_file(path: &Path, contents: &str) -> Result<(), Failure> {
    fs::write(path, contents).map_err(|error| {
        eprintln!("error: cannot write `{}`: {error}", path.display());
        Failure(EXIT_COMPILE_ERROR)
    })
}

/// Generate an object file and link it with the runtime into an executable at `output`.
fn link_executable(program: &hir::Program, sources: &SourceMap, opt: OptLevel, output: &Path) -> Result<(), Failure> {
    let runtime = find_runtime()?;
    let temp = TempDir::new()?;
    let object = temp.path().join(format!("{}.o", program.module));
    lang_codegen::emit_object(program, sources, opt, &object).map_err(internal_error)?;

    let linker = env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let status = Command::new(&linker)
        .arg(&object)
        .arg(&runtime)
        .arg("-o")
        .arg(output)
        .arg("-Wl,--gc-sections")
        .args(SYSTEM_LIBRARIES)
        .status()
        .map_err(|error| {
            eprintln!("error: could not run the linker `{linker}`: {error}");
            eprintln!("  = help: install a C compiler such as gcc or clang, or set `CC` to one");
            Failure(EXIT_COMPILE_ERROR)
        })?;
    if !status.success() {
        eprintln!("error: linking failed; `{linker}` exited with {status}");
        return Err(Failure(EXIT_COMPILE_ERROR));
    }
    Ok(())
}

/// The runtime library sits next to the compiler executable, or wherever `<PREFIX>RUNTIME` says.
fn find_runtime() -> Result<PathBuf, Failure> {
    let variable = env_var("RUNTIME");
    if let Some(path) = env::var_os(&variable) {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        eprintln!("error: `{variable}` is set to `{}`, which is not a file", path.display());
        return Err(Failure(EXIT_COMPILE_ERROR));
    }
    let directory = env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)).unwrap_or_default();
    let candidate = directory.join(RUNTIME_LIBRARY);
    if candidate.is_file() {
        return Ok(candidate);
    }
    eprintln!("error: cannot find the runtime library `{RUNTIME_LIBRARY}` in `{}`", directory.display());
    eprintln!("  = help: build it with `cargo build --workspace`, or set `{variable}` to its path");
    Err(Failure(EXIT_COMPILE_ERROR))
}

/// A private temporary directory, removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Result<TempDir, Failure> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = format!("{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed));
        let path = env::temp_dir().join(CACHE_DIR_NAME).join(unique);
        fs::create_dir_all(&path).map_err(|error| {
            eprintln!("error: cannot create the build directory `{}`: {error}", path.display());
            Failure(EXIT_COMPILE_ERROR)
        })?;
        Ok(TempDir(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
