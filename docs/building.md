# Building and using the compiler

This page covers:
- what to install before building;
- how to build and install with `make`;
- what every command and flag of the `weby` compiler does;
- the environment variables the compiler and compiled programs read.

The language itself is taught in [examples.md](examples.md).

## Supported platforms

Linux on x86-64 only. Two parts of the implementation are specific to it:
- the runtime switches between tasks with a few lines of x86-64 assembly and waits for sockets with `epoll`;
- the code generator is built with LLVM's x86 target only.

macOS, Windows and ARM are not supported natively yet. On Windows and macOS the installers below run the compiler in a Linux environment (WSL 2 and a Lima VM), so you can use it there; the programs it builds are Linux programs.

## Installing with a script

Each script assumes nothing is installed: they add LLVM 19, the Boehm collector, a C toolchain and Rust, build the compiler and runtime from this checkout, and install the `weby` command. Each asks before it changes anything (`--yes` skips the question).

**Debian, Ubuntu and other apt-based Linux (x86-64).** Installs the packages with `apt` (adding [apt.llvm.org](https://apt.llvm.org) if the release has no `llvm-19-dev`) and Rust with rustup, then runs `make install`:

```bash
scripts/install.sh
```

`--prefix DIR` chooses where `weby` goes (default `~/.local`), `--dry-run` prints what would be done, and `--no-system-packages` skips `apt` and `sudo` for a machine where the packages are already there.

**Windows 10 (2004 or later) and 11.** In an administrator PowerShell, which is needed once to install WSL 2 and may ask for a restart (restart, then run the script again):

```powershell
powershell -ExecutionPolicy Bypass -File scripts\install.ps1
```

It installs Ubuntu 24.04 in WSL 2 if it is missing, runs the Linux installer in it, and adds `weby.cmd` to your PATH. `weby.cmd` runs the compiler in WSL in the folder you are in; programs it builds are Linux executables, so use `weby run app.wb`.

**macOS.** Needs Homebrew (the script offers to install it):

```bash
scripts/install-macos.sh
```

It installs [Lima](https://lima-vm.io), creates an Ubuntu 24.04 x86-64 VM named `weby`, runs the Linux installer in it, and adds a `weby` command to `~/.local/bin` that runs the compiler in the VM in the folder you are in. Your home folder is shared with the VM. On Apple Silicon the VM emulates x86-64, so installing takes a long time and programs run several times slower than natively; on an Intel Mac it runs at full speed.

Only `scripts/install.sh` is run by continuous integration (as a dry run). The Windows and macOS scripts have not been run on those systems yet; if one fails, the steps it prints are the manual ones in this page.

## Required packages

| What | Version | Why | Debian / Ubuntu package |
|---|---|---|---|
| Rust (`rustc` and `cargo`) | 1.85 or newer | The compiler and runtime are written in Rust | from [rustup.rs](https://rustup.rs), or `rustc` and `cargo` if new enough |
| LLVM development files | 19 exactly | Code generation. The compiler links the shared `libLLVM-19.so` | `llvm-19-dev` |
| A C compiler | any recent `gcc` or `clang` | Linking every compiled program, and building the `ring` cryptography library behind TLS | `gcc` (or `build-essential`) |
| Boehm garbage collector | 8.x | The garbage collector every compiled program uses | `libgc1` |

On Debian 13 or Ubuntu:

```bash
sudo apt install build-essential llvm-19-dev libgc1
```

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

**If your distribution has no `llvm-19-dev`:** [apt.llvm.org](https://apt.llvm.org) has LLVM 19 packages for Debian and Ubuntu releases.

**On other distributions,** install:
- LLVM 19 with its shared library and `llvm-config`;
- a C compiler;
- the Boehm GC runtime library, which provides `libgc.so.1`.

Then set `LLVM_SYS_191_PREFIX` to the directory that contains LLVM 19's `bin/llvm-config`. The default is `/usr/lib/llvm-19`, set in [`.cargo/config.toml`](../.cargo/config.toml); a value in the environment overrides it.

The compiler links programs against `libgc.so.1` itself, not `libgc.so`, so the GC's `-dev` package is not needed.

You don't need OpenSSL or the system's certificate store. TLS is `rustls`, with the Mozilla root certificates compiled in.

### Optional packages

| What | Needed for |
|---|---|
| `make` | The [Makefile](../Makefile) shortcuts; every target is a plain `cargo` or script command you can run yourself |
| `git` | `weby fetch`, which clones the git dependencies in `weby.toml` |
| Python 3 | The benchmark scripts in `bench/` |
| Go | The benchmark comparisons ([bench/README.md](../bench/README.md)) |
| PostgreSQL | The database test, which runs only when `WEBY_TEST_DATABASE_URL` is set ([tests/db/README.md](../tests/db/README.md)) |
| `gdb`, `perf` | Debugging and profiling compiled programs, which carry DWARF line information and frame pointers |

### Checking what is installed

```bash
make check-deps
```

This runs [`scripts/check-deps.sh`](../scripts/check-deps.sh), which checks each requirement and prints one line per requirement, for example:

```text
Required
  ok       Linux on x86-64      Linux x86_64
  ok       Rust 1.85 or newer   rustc 1.100.0
  ok       LLVM 19              19.1.7 in /usr/lib/llvm-19
  ok       C compiler           /usr/bin/cc
  ok       Boehm GC             /usr/lib/x86_64-linux-gnu/libgc.so.1
```

It exits with status 1 if anything required is missing.

### What compiled programs need

A compiled program is a single executable that links `libgc.so.1` and the C library dynamically, and everything else statically. Running it doesn't need LLVM, Rust or the compiler, so a server that runs one needs only `libgc1`:

```bash
sudo apt install libgc1
```

To see exactly what an executable loads, run `ldd ./app`.

## Building

```bash
make
```

This builds the compiler `target/release/weby` and the runtime library `target/release/liblang_rt.a`. Every program is linked with the runtime library.

The compiler looks for `liblang_rt.a` in the directory the compiler itself is in, so the two must be built together. That is why the `make` targets, and the `cargo` commands under them, build the whole workspace.

| Target | Does | The same with cargo |
|---|---|---|
| `make` or `make build` | Build the compiler and runtime, optimised | `cargo build --workspace --release` |
| `make debug` | Build them unoptimised, into `target/debug`; quicker to build | `cargo build --workspace` |
| `make run FILE=app.wb [ARGS="..."]` | Build, then compile and run a program | `target/release/weby run --release app.wb ...` |
| `make program FILE=app.wb [OUT=app]` | Build, then compile a program to an executable | `target/release/weby build --release app.wb -o app` |
| `make install` | Build, then install the `weby` command into `~/.local`, or wherever `PREFIX` says | (below) |
| `make link` | Build, then make the `weby` command run this checkout's build, so each rebuild updates it | (below) |
| `make uninstall` | Remove what `make install` or `make link` installed | |
| `make test` | Run the whole test suite | `cargo test --workspace` |
| `make lint` | Run clippy with warnings as errors, and check the Rust formatting | `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all --check` |
| `make fmt` | Format the Rust sources | `cargo fmt --all` |
| `make stress [RUNS=n]` | Run the concurrency programs many times | `tests/stress.sh [n]` |
| `make bench`, `bench-http`, `bench-ws` | The benchmarks in [bench/README.md](../bench/README.md); pass options with `ARGS="..."` | `python3 bench/run.py ...` |
| `make check-deps` | Check the required packages | `scripts/check-deps.sh` |
| `make clean` | Remove everything built | `cargo clean` |
| `make help` | List the targets | |

The variables can be set on the command line, as in `make install PREFIX=/opt/weby`:

| Variable | Default | Meaning |
|---|---|---|
| `PROFILE` | `release` | `release` or `debug`. It decides which build `build`, `run`, `program` and `install` use. It also decides whether `run` and `program` pass `--release` |
| `PREFIX` | `~/.local` | Where `make install` puts things |
| `BINDIR`, `LIBDIR` | `$(PREFIX)/bin`, `$(PREFIX)/lib/weby` | The two directories `make install` uses |
| `DESTDIR` | empty | Prepended to the install paths, for building packages |
| `CARGOFLAGS` | empty | Extra arguments for every cargo command, for example `--offline` or `--locked` |
| `CARGO`, `PYTHON` | `cargo`, `python3` | The commands used |

**Release or debug.** Build the compiler in release mode for anything you measure or deploy. `weby build --release` optimises the program's own code, but every program also contains the runtime library: the HTTP server, the scheduler, JSON and the collector's interface. That library is optimised only when cargo built it in release mode. A program compiled with `target/debug/weby` runs several times slower, whatever flags it was compiled with. The debug build is for working on the compiler, where build time matters more. The release build links with link-time optimisation in one code-generation unit (`[profile.release]` in `Cargo.toml`), so that the runtime's helpers inline into the functions programs call: a clean release build takes about 9 s longer for it, and programs run 2% to 6% faster and are a third smaller.

### Installing the `weby` command

Either target builds the compiler first, then puts a `weby` command in `~/.local/bin`:

```bash
make install
```

```bash
weby server.wb -o server
```

| | `make install` | `make link` |
|---|---|---|
| What `~/.local/bin/weby` is | A link to a copy in `~/.local/lib/weby/` | A link to `target/release/weby` in this checkout |
| After pulling changes and rebuilding | Run `make install` again | Nothing to do: `make` updates the command |
| Removing the checkout | The command keeps working | The command stops working |

`make install` copies two things into `$(PREFIX)/lib/weby/`: the compiler and the runtime library. The compiler follows the link in `bin` to find the runtime library next to it. The standard library is compiled into the compiler, so nothing else is installed.

`~/.local/bin` must be on your `PATH`. Both targets check, and if it is not they print the line to add to `~/.bashrc` or `~/.profile`:

```bash
export PATH="$HOME/.local/bin:$PATH"
```

To install system-wide, set the prefix:

```bash
sudo make install PREFIX=/usr/local
```

The `sudo make install` command still runs the build first, so build first as yourself (`make`), or the build output in `target/` will belong to root.

To put the runtime library anywhere else, set `WEBY_RUNTIME` to its path.

## The `weby` command

| Command | Does |
|---|---|
| `weby [--release] FILE [-o OUT]` | Short for `weby build`: with no command, `build` is assumed |
| `weby run [--release] FILE [ARGS...]` | Compile to a temporary executable, run it with `ARGS`, delete it |
| `weby build [--release] [-o OUT] [--emit FORM] FILE` | Compile to an executable, or write an intermediate form |
| `weby check FILE` | Parse and type-check, reporting errors, without generating code |
| `weby test [--release] [--filter TEXT] [PATHS...]` | Run the `@test` functions |
| `weby fmt [--check] [PATHS...]` | Rewrite source files in the standard layout |
| `weby fetch [--update] [FILE]` | Fetch the project's git dependencies |
| `weby lsp` | Run the language server on standard input and output |
| `weby --help`, `weby COMMAND --help` | List the commands, or one command's flags |
| `weby --version` | Print the compiler's version |

`FILE` is the file containing `main`. Other modules are found from its `import` lines: in the same directory, in the standard library, or in a dependency named in `weby.toml` (the [README](../README.md#building-and-running) shows the format). Source files must end in `.wb`.

### `--release`

| | Without `--release` | With `--release` |
|---|---|---|
| LLVM optimisation | `mem2reg` only: values kept in registers, nothing else | LLVM's standard `-O2` pipeline: inlining, loop optimisation, vectorisation |
| Compile time | Shortest | Longer |
| `--emit=llvm-ir` | Readable, close to the source | Optimised |
| Debug information | DWARF line information for every function and expression | The same |
| Frame pointers | Kept | Kept, so `perf record -g` works; they cost about 10% on code that is nothing but calls |
| CPU | Generic x86-64; the executable runs on any x86-64 machine | The same: there is no `-march=native` |

A program behaves the same in both modes; only its speed differs:
- integer division by zero and out-of-range indexes panic;
- integer overflow wraps, as in Go ([D16](08-decisions-and-risks.md));
- a panic prints a backtrace with source lines. Set `WEBY_BACKTRACE=0` to leave it out.

The end-to-end tests run every program in both modes.

### `weby run`

```bash
weby run --release server.wb 8080 --verbose
```

**Arguments:** everything after `FILE` goes to the program, even arguments that start with `-`. So the compiler's own flags must come *before* the file. In the example, the program gets `8080 --verbose`, which `os.args()` returns.

**Exit status:** the exit status is the program's own. A program killed by a signal exits with 128 plus the signal number, as a shell would report it.

### `weby build`

```bash
weby build --release server.wb -o server
```

`build` is the default command, so this is the same:

```bash
weby --release server.wb -o server
```

`build` is assumed only when the first argument is a `.wb` file or a flag. A mistyped command such as `weby biuld` is still reported as unknown.

`-o` / `--output` names the output. Without it, an executable is written to the current directory, named after the source file without `.wb`.

**Linking:** the executable is linked by running the C compiler. That is `cc`, or whatever `CC` names, for example `CC=clang`. The command is:

```text
cc PROGRAM.o liblang_rt.a -o OUT -Wl,--gc-sections -l:libgc.so.1 -lgcc_s -lutil -lrt -lpthread -lm -ldl -lc
```

`--gc-sections` drops the parts of the runtime the program does not use.

`--emit=FORM` writes an intermediate form instead of an executable. It is for looking inside the compiler and for bug reports:

| `--emit=` | Output | Where |
|---|---|---|
| `tokens` | The token stream, including the indent and dedent tokens, one per line with its position | Standard output |
| `ast` | The syntax tree after parsing | Standard output |
| `hir` | The type-checked program, with every type and call resolved | Standard output |
| `llvm-ir` | LLVM IR as text, optimised if `--release` is given | Standard output, or the `-o` file |
| `obj` | A native object file, not yet linked | `-o`, or the source name with `.o` |

### `weby check`

`weby check FILE` runs the parser and type checker on `FILE` and everything it imports. It prints the same errors as `build` and generates no code. It is the quickest way to check a program, and the language server runs the same checks as you type.

### `weby test`

```bash
weby test
```

```bash
weby test tests/ --filter parse --release
```

| Option | Effect |
|---|---|
| `PATHS` | Files, or directories searched recursively. The default is the current directory. Every `@test` function in them is found |
| `--filter TEXT` | Runs only the tests whose name contains `TEXT` |
| `--release` | Compiles the tests with optimisation |

Each test runs in its own task, and a panic fails only that test. The summary line is `test result: ok. 3 passed; 0 failed`. The command exits with status 1 if any test failed.

### `weby fmt`

`weby fmt` rewrites `.wb` files in the standard layout, in place. It formats the given files or directories, or the current directory if none is given.

With `--check`, nothing is changed: the command lists the files that are not formatted and fails if there are any. Use `--check` in CI.

### `weby fetch`

`weby fetch` clones the git dependencies listed in `weby.toml` into `.weby/deps`. It records each one's commit in `weby.lock`, so later builds use the same code.

`--update` moves each dependency to its `rev` (or to the newest commit, if there is no `rev`) and rewrites the lock file.

`build`, `run` and `check` fetch missing dependencies themselves, so `fetch` is mostly for updating.

### `weby lsp`

`weby lsp` is the language server, run by editors rather than by hand. It speaks the Language Server Protocol on standard input and output. It reports errors as you type and supports hover, go-to-definition and formatting. The Zed extension starts it automatically; [editors/README.md](../editors/README.md) covers each editor.

### Exit statuses

| Status | Meaning |
|---|---|
| 0 | Success |
| 1 | An error in the program being compiled (the message says where), a file that cannot be read, or a failed link or test |
| 2 | A command-line mistake, such as an unknown command or flag |
| 101 | A bug in the compiler (`internal compiler error: ...`); please report it with the program |
| other | From `weby run`: the program's own status |

## Environment variables

**When building the compiler:**

| Variable | Effect |
|---|---|
| `LLVM_SYS_191_PREFIX` | The LLVM 19 installation to build against (default `/usr/lib/llvm-19`, from `.cargo/config.toml`) |

**When compiling a program:**

| Variable | Effect |
|---|---|
| `CC` | The C compiler used to link (default `cc`) |
| `WEBY_RUNTIME` | The path of `liblang_rt.a`, instead of looking next to the compiler. Useful for testing a runtime built some other way |
| `WEBY_RUNTIME_DEBUG` | Any value keeps the runtime's debug information in the executable, which is otherwise removed with `objcopy --strip-debug` while linking (4.4 MB down to 0.5 MB for a small program). The program's own line tables are always kept. Useful when debugging the runtime itself |

**When running a compiled program.** These are read when the program starts, so the same executable can be tuned without recompiling:

| Variable | Default | Effect |
|---|---|---|
| `WEBY_WORKERS` | the number of CPUs the process may use | Worker threads that run tasks. The default respects `taskset` and container CPU limits |
| `WEBY_TASK_STACK` | `1048576` (1 MiB) | Bytes of stack reserved for each task; at least 64 KiB. Only the pages a task touches use memory |
| `WEBY_GC_FREE_SPACE_DIVISOR` | `2` | How often the collector runs once the live data is larger than `WEBY_GC_MIN_ALLOC`: after allocating about half the live data. `1` collects about half as often, like Go's default, for up to 40% more memory; `3` uses a little less memory for more collection time |
| `WEBY_GC_MIN_ALLOC` | `4194304` (4 MiB) | The least the program allocates between collections, in bytes, however little live data it has. Go's minimum heap is the same. Lower values save up to that much memory in small programs, at the cost of many more collections in busy ones |
| `WEBY_GC_INITIAL_HEAP` | the collector's default | Bytes of heap to reserve at start, to skip the first few collections |
| `WEBY_GC_STRESS` | off | Any value other than `0` collects before every allocation. Slow; it finds missing garbage-collector roots. `cache` instead keeps the per-thread free lists in use but short, and collects before every refill: it finds objects on a free list that the collector cannot see |
| `WEBY_PARALLEL_MARK_HEAP` | `33554432` (32 MiB) | Once the heap is this large, a program that has started no tasks lets the collector mark with up to four threads (a program with tasks does from its first `spawn`). Less wall time for more CPU time; `0` keeps marking on one thread |
| `WEBY_ALLOC_STATS` | off | Any value other than `0` prints, at exit, the bytes allocated, the number of collections and the heap size to standard error |
| `WEBY_LOG` | `info` | The lowest `log` level written: `debug`, `info`, `warn`, `error` or `off` |
| `WEBY_BACKTRACE` | on | `0` leaves the backtrace out of panic messages |
| `SSL_CERT_FILE` | none | A PEM file of extra root certificates for outgoing TLS, added to the built-in Mozilla roots |

**When running the test suite:**

| Variable | Effect |
|---|---|
| `WEBY_BLESS=1` | Rewrites the expected compiler messages in `tests/ui` instead of comparing with them |
| `WEBY_TEST_DATABASE_URL` | A PostgreSQL database the database test may use; that test is skipped without it |

## When something goes wrong

| Message | Cause and fix |
|---|---|
| `No suitable version of LLVM was found` while building | LLVM 19 is missing or somewhere else. Install `llvm-19-dev`, or set `LLVM_SYS_191_PREFIX` |
| `cannot find the runtime library liblang_rt.a` | The compiler was built without the runtime. Build with `make` or `cargo build --workspace`, not `cargo build -p lang_cli`; or set `WEBY_RUNTIME` |
| `could not run the linker cc` | No C compiler. Install `gcc`, or set `CC` |
| `cannot find -l:libgc.so.1` when linking | Install `libgc1` |
| `error while loading shared libraries: libgc.so.1` when running a program | The machine running the program needs `libgc1` |
| A program is much slower than expected | It was compiled by a debug build of the compiler, whose runtime is not optimised. Use `make` (release) and `--release` |
