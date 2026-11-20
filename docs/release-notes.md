# Release notes

The first release of Weby, a compiled, Python-shaped language for web backends. `weby --version` reports 1.0.0. This page says what the language has, what it does not, and how to install it. The state of each part, with the reasoning behind the choices, is in the [roadmap](06-roadmap.md) and the [decision log](08-decisions-and-risks.md); [Weby by example](examples.md) teaches the language, and every program in it is compiled and run by the test suite.

## What the language has

**The language.**
- Python's layout and much of its syntax: functions, `if`/`elif`/`else`, `while`, `for`, `match` (as a statement, and as a value on the right of `=` or `return`), `with`, comprehensions for lists, dicts and sets, f-strings with `!r`, raw strings, lambdas, nested functions, one-line blocks (`if done: return`), constants worked out when compiling, and `del`.
- Static types written as Python type hints, with local inference. Every parameter and return type is written; variables take the type of their first value. There is no implicit conversion between numbers.
- Structs and `impl` blocks, enums with fields, traits with default methods and required traits, trait objects (taken apart again with `case Circle(c):`), `@derive` for `Eq`, `Hash`, `Display`, `Copy`, `Ord` and `Json`, `@frozen` structs.
- Generics for functions, structs, enums, `impl` blocks and methods (a method can have type parameters of its own), with bounds, compiled once per set of types.
- Errors are values: `Result[T]`, `T?` for optional values, `?` to pass an error on, and a panic (with a backtrace of your functions and lines) for what cannot be recovered.
- Types: `int` (64 bits, wrapping), `float`, `bool`, `str`, `list`, `dict` (insertion ordered), `set`, tuples, `bytes`, sized numbers for C, optionals, and the immutable `tuple[T, ...]` and `frozendict` for sharing between tasks.
- Modules and packages (`import`, `pub`/`priv`), a project manifest with git and path dependencies, C functions through `@extern`.

**The standard library.** `io` and `os` (files, standard streams, the environment), `json` (typed encoding and decoding with `@derive(Json)`, and a dynamic `json.Value`), `http` (server, client, routes, typed handlers, WebSockets, TLS), `re` (regular expressions that match in linear time), `time`, `log`, `postgres` (a driver with pooling, transactions and typed rows), and about thirty more methods of `str`, `list` and `dict` than a minimal core would have.

**Concurrency.** `spawn` starts a task; tasks are fibers scheduled over one worker thread per core. `Channel`, `Mutex` (reached only through `with m.lock() as value:`), `Atomic`. The compiler rejects programs that would share a changeable value between tasks, and says how to fix them.

**The web stack.** Handlers can be written with annotations, `@http.get("/todos/{id}")`, and typed parameters that the compiler binds from the path, query, headers, cookies and a JSON body, with `400`s generated for bad input. WebSocket endpoints with `@http.ws`. HTTPS with `rustls`. Graceful shutdown on SIGTERM.

**Tooling.** `weby run`, `build`, `check`, `test` (for `@test` functions), `fmt`, `fetch`, `explain` (for an error code) and `lsp`. The language server gives diagnostics, hover, go to definition, completion and formatting. Editor support for VS Code, Vim, Neovim, Helix and Zed is in `editors/`. The messages point at the source, suggest the nearest name, and every code has an entry in the [error reference](errors.md).

**Performance.** On this repository's benchmarks every program runs faster than Go's version (between 1.1 and 3.0 times), and the HTTP endpoints serve 1.8 to 3.1 times Go's request rate at 256 connections with about 40% of its CPU per request. Idle WebSocket connections cost about 19 KiB each. A small program's executable is 0.5 MB and the benchmark web server's 1.3 MB. The numbers, how they were measured and what limits them are in [web-performance.md](web-performance.md) and [bench/README.md](../bench/README.md). `bench/run.py --check` and `bench/http/counts.py --check` fail on a regression.

## What changed in the last round

The [release polish plan](11-release-polish-plan.md) closed the gaps a Python programmer meets in the first hour:

- syntax: one-line blocks, raw strings, `del`, constant expressions, `!r` and `!s`, `print(..., sep=, end=, flush=)`, `match` as a value;
- `str`, `list` and `dict` gained about thirty methods written in the language's own prelude, and the functions `ord`, `chr`, `reversed` and `list(...)`;
- `set[T]`, with literals, comprehensions, operators, methods and JSON;
- methods with type parameters of their own, and `case Circle(c):` on a trait object;
- a clear `panic: stack overflow` and exit status 101 instead of a segmentation fault;
- completion in the language server, a warning for unused imports, `weby explain`, and executables four to eight times smaller.

## Known limitations

These are choices made for this release, each with its reason in the decision log. None changes a program that compiles.

**Platform.** Linux on x86-64 only (D13). The context switch is assembly for that CPU, the poller is `epoll`, and the compiler is built with LLVM's x86 target.

**Language.**
- No generic traits: iteration, `with` and the like are method protocols (`next`, `iter`, `enter`, `exit`; D34, D38). Traits cannot have associated types.
- A closure can read the enclosing function's variables but not assign them (D35). Comparison chains (`a < b < c`), `*args` and `**kwargs`, `global` and `nonlocal`, and raw f-strings are errors, each with a message that says what to write.
- `empty = []` needs a type hint, `items: list[int] = []`, because a type is not inferred from a later use (D94).
- No bytes literals (`b"..."`; write `"...".encode()`) and no immutable set (`frozenset`); share a `set` through a `Mutex` (D91, D93).
- A set prints and iterates in insertion order, where Python's sets have an order of their own, and its methods take sets, not any iterable.
- Strings are indexed by byte offset, so `len(s)` is the number of bytes (D22).
- Integers wrap on overflow (D16).

**Runtime.**
- There is no preemption: a task that never waits holds its worker thread until it does, and as many such tasks as there are cores stop the program's other tasks. Keep handlers short, or have a long computation wait now and then, with `time.sleep(0.001)` or a channel operation. A check at every loop iteration was measured and slows tight loops by 25% (D95).
- There is no blocking pool: file I/O and `@extern` calls run on the worker that makes them (D66).
- A panic in a task stops the task, not the program (a handler's becomes a `500`), but a `with` block it leaves does not run its `exit` (D64); mutexes it held are released. A stack overflow ends the whole program.
- The collector is Boehm's, conservative and not moving; memory for allocation-heavy programs can be above Go's (D86). Collections of structs hold pointers, not inline values.

**Tooling.**
- Debug information has line tables and function names but not variables or types (D67): a debugger can break, step and show a backtrace, not print locals.
- `weby fmt` keeps your line breaks; it does not fit lines to a width (D68). The language server checks the whole program on each change (D70).
- Dependencies come from git or paths with a lock file; there is no registry and no version solving (D71).
- Testing: the runtime has not been run under ThreadSanitizer, which needs a nightly Rust toolchain (D45). JSON and WebSocket conformance suites are run by hand, not in continuous integration (D81).

## Installing

You need Linux on x86-64, LLVM 19, the Boehm collector (`libgc`), a C compiler to link with, and a Rust toolchain to build the compiler. [building.md](building.md) lists the packages and the steps:

```bash
make
```

```bash
make install
```

The first builds the compiler and the runtime library; the second puts the `weby` command in `~/.local/bin` (`PREFIX=... make install` for somewhere else). Then:

```bash
weby run hello.wb
```
