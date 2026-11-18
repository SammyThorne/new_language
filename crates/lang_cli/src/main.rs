//! The compiler's command-line driver: runs the pipeline in docs/03-compiler-architecture.md
//! and links the result with the runtime library.

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
#[command(name = CLI_NAME, version, about = format!("Compiler for the {LANGUAGE_NAME} programming language"))]
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
    let cli = Cli::parse();
    let result = match cli.command {
        Cmd::Build { file, output, release, emit } => build(&file, output, opt_level(release), emit),
        Cmd::Run { file, release, args } => run(&file, opt_level(release), &args),
        Cmd::Check { file } => frontend(&file, None).map(|_| ExitCode::SUCCESS),
    };
    result.unwrap_or_else(|Failure(code)| ExitCode::from(code))
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

/// Lex, parse and type-check one file. Prints diagnostics. Returns the program, or `None` when
/// `emit` asked for an earlier stage, which has then been printed.
fn frontend(file: &Path, emit: Option<Emit>) -> Result<Option<(hir::Program, SourceMap)>, Failure> {
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

    // Type errors in a file that did not parse are mostly noise (a function whose header failed
    // to parse is "unknown" at every call), so checking waits for a clean parse.
    let program = if diags.has_errors() {
        None
    } else {
        let module_name = file.file_stem().map_or("main".into(), |stem| stem.to_string_lossy().into_owned());
        lang_sema::check(&module, &mut sources, &module_name, &mut diags)
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
