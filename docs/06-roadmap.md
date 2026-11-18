# 06 — Roadmap

Each milestone ends with a program that runs. A milestone is done when its exit program compiles, runs correctly, and is in the test suite; not when a feature list is ticked.

Sizes are relative effort for one experienced developer: **S** about a week, **M** two to three weeks, **L** four to six weeks. They are planning aids, not commitments; language projects reliably overrun in the type checker and the runtime.

## Summary

| # | Milestone | Size | Exit program |
|---|---|---|---|
| M0 | Skeleton and toolchain | S | A hard-coded "hello" goes through LLVM, links with the runtime, and runs |
| M1 | Core language | L | Recursive Fibonacci, FizzBuzz, a prime sieve using only scalars |
| M2 | Heap data | L | Word-frequency counter over a text file's contents using structs, lists, dicts, strings |
| M3 | Traits, enums, errors | L | A shapes program with dynamic dispatch; an expression evaluator with enums, `match` and `Result` |
| M4 | Generics and closures | L | A generic `Stack[T]`, `map`/`filter` with lambdas, a user-defined iterator |
| M5 | Modules and FFI | M | The M3 evaluator split across three files plus a file-reading CLI tool |
| M6 | Concurrency (stage 1) and sharing checks | L | A parallel worker pool with channels; a mutex-protected counter hit by 100 tasks; a set of programs that must fail to compile because they share mutable data |
| M7 | JSON | M | Round-trip a nested typed document; walk an unknown document dynamically |
| M8 | HTTP server, route annotations, typed handlers | L+ | The todo REST service from [05](05-standard-library.md#http), written with `@http` annotations, passing an HTTP test script |
| M9 | WebSockets | M | The chat hub from [05](05-standard-library.md#websockets), declared with `@http.ws`, passing Autobahn |
| | **Prototype complete** | | |
| M10 | Fibers and poller (stage 2) | L | The same services, unchanged source, holding 50,000 idle WebSocket connections |
| M11 | Tooling | L | Debug info, formatter, test runner, language server basics |
| M12 | Performance and ecosystem | ongoing | Go parity on the benchmark suite; HTTP client; PostgreSQL driver; TLS |

Order of dependency:

```
                               ┌─ M6 ─┐
M0 ─ M1 ─ M2 ─ M3 ─ M4 ─ M5 ───┤      ├─ M8 ─ M9 ══ prototype ══╦═ M10 ─ M12
                               └─ M7 ─┘                         ╚═ M11
```

M6 and M7 are independent of each other and can be done in either order or in parallel. M11 can run alongside M10. The hard dependencies behind the linear part: enums (M3) underpin `Option` and `Result` everywhere after; closures (M4) are needed by `spawn` and by route handlers; the import machinery (M5) is how standard library modules are found.

## Progress

As of 2026-11-18.

| Milestone | State | Notes |
|---|---|---|
| M0 | Done | Workspace, runtime, LLVM object emission, linking, end-to-end test harness. Continuous integration is not set up, because the repository has no remote yet |
| M1 | Done | Everything listed below, with `float` formatted exactly as CPython does. The first benchmark is recorded in [bench/README.md](../bench/README.md) |
| M2 | Done | Everything listed below. Gate 1 passed. Sets and `str(struct)` are not included: sets were not in the milestone, and printing structs needs `Display` (M3) |

Exit programs, all in `tests/run/`: `fizzbuzz.wb`, `primes.wb` and `fib.wb` (M1), and `word_frequency.wb` (M2). The binary-trees benchmark is in `bench/`.

**Gate 1 result.** Allocation-heavy code (binary trees, depth 18) runs 20.6x faster than CPython 3.13, against a target of 10x. All 20 test programs pass in the GC stress mode. The weakest benchmark is string-and-dict-heavy work at 1.9x CPython; [bench/README.md](../bench/README.md) explains why and what fixes it.

Choices made during M1 and M2 that the earlier documents did not settle are recorded in [08](08-decisions-and-risks.md) as D21 to D25.

## Milestones in detail

### M0 — Skeleton and toolchain (S)

- Cargo workspace with the crates from [03](03-compiler-architecture.md#repository-layout).
- `inkwell` building against the installed LLVM 19; confirm the feature flag and record the exact versions.
- `lang_rt` as a static library with one function (print a string) and a C-ABI entry that calls the program's `main`.
- `weby build` that constructs a trivial LLVM module by hand, emits an object, and links it.
- End-to-end test harness: compile a `.wb` file, run it, compare standard output and exit code against expectations embedded in comments.
- Continuous integration running build, tests, formatting and lints.

Exit: `weby run hello.wb` prints a line. The source file is not parsed yet; the point is that every stage of the toolchain is connected.

### M1 — Core language (L)

- Lexer with the full layout algorithm; parser for functions, statements and expressions; diagnostics rendering.
- Types: `int`, `float`, `bool`, and `str` literals that can be printed.
- Variables by assignment, definite-assignment analysis, `if`/`elif`/`else`, `while`, `for` over `range`, `break`, `continue`, `return`.
- Functions with typed parameters, recursion, forward references.
- Type checker for the above; HIR; codegen; panics with source locations; debug and release modes.
- `weby check` and the `--emit` dumps.
- First benchmark: recursive Fibonacci and a numeric loop against CPython.

Exit: the three scalar programs run; twenty or more diagnostics have golden tests; the first benchmark numbers are recorded.

### M2 — Heap data (L)

- Runtime allocation entry point, type descriptors, Boehm integrated, GC stress mode.
- Structs, the generated constructor, `impl` blocks with inherent methods, field access and mutation.
- `str` operations, f-strings, `list[T]`, `dict[K, V]`, tuples and tuple unpacking, `for` over lists and dicts, `in`, slicing.
- `T?` as a built-in with `None`, `unwrap`, `unwrap_or` (full enum machinery comes in M3).
- Built-in collections are special-cased generics.

Exit: the word-frequency program; an allocation benchmark (binary trees) recorded against CPython.

### M3 — Traits, enums, errors (L)

- `trait`, `impl Trait for Type`, default methods, required traits, coherence checks.
- Trait objects: vtables, the struct-to-trait coercion, dynamic calls.
- `enum` with payloads; `match` with exhaustiveness checking and decision-tree codegen.
- `Option` and `Result` redefined as real enums; `?`; implicit `Some`/`Ok` wrapping; the `Error` trait; unused-`Result` error.
- `@derive` for `Eq`, `Hash`, `Display`, `Copy`.
- `@frozen` structs (assignment to a field is rejected).
- The arithmetic traits `AddNumber`, `SubNumber`, `MulNumber`, implemented by the built-in number types. Operators on those types still compile to machine instructions.

Exit: the shapes and evaluator programs. This is the point where the language's character is visible; a good moment to review syntax with fresh eyes before more code depends on it.

### M4 — Generics and closures (L)

- User generic functions, structs, enums and impls with bounds; monomorphisation; generic impls.
- Static dispatch through bounds; `Ord`; `Number`; arithmetic traits on user structs; `Iterable`/`Iterator`; `for` over user iterators.
- `lambda`, nested functions with capture, bound methods as values, function types.
- Default and keyword arguments; list comprehensions; flow narrowing for options.

Exit: the generic container and iterator programs; a sort benchmark with a comparator closure.

### M5 — Modules and FFI (M)

- `import`, `from ... import`, `as`; project-root and standard-library lookup; `pub` and `priv` visibility; cycle detection.
- Standard library split into modules; `io` and `os`.
- `@extern` for binding C functions; sized numeric types.
- `with` and the `Scoped` trait; files.

Exit: a multi-file project builds with one command.

### M6 — Concurrency, stage 1, and sharing checks (L)

- `spawn`, `Task[T]`, thread registration with the collector, task cap.
- `Channel[T]`, `Mutex[T]`, `Atomic`, `time.sleep`.
- The `Shareable` property and its enforcement at `spawn`, channels, mutexes and lock guards, with diagnostics that say how to fix the program.
- Immutable collections for read-only shared data: `tuple[T, ...]`, `frozenset[T]`, a frozen `dict`.
- Stress tests that run under the GC stress mode and under ThreadSanitizer for the runtime.

Exit: the worker pool and counter programs, run a thousand times in CI without failure; every program in the "must not compile" set is rejected with the expected message.

### M7 — JSON (M)

- Runtime scanner and writer.
- `json.Value`, `json.parse`, dynamic indexing and accessors.
- `@derive(Json)` for structs and enums, `@json(name=...)`, error paths.
- JSON benchmark against Go's `encoding/json` and Python's `json`.

Exit: the round-trip and dynamic-walk programs; a standard JSON conformance corpus passes.

### M8 — HTTP server, route annotations, typed handlers (L+)

- `net` listener and stream; HTTP/1.1 parsing, keep-alive, limits, timeouts.
- Stage A, call style: router with path parameters; `Request` and `Response`; response helpers; error mapping; middleware; groups; graceful shutdown; `log`.
- Stage B, annotations: `@http.get` and siblings; compile-time binding of path, query and body parameters; return-type encoding; generated adapters; `server.mount`; the route-specific compile errors. Stage B is compiler work only and adds nothing to the runtime.
- HTTP benchmarks (plain text and JSON) against Go `net/http` and a Python framework.

Exit: the todo service passes a scripted HTTP test in both styles; benchmark numbers are recorded. **Gate 2** is evaluated here.

This is the largest milestone. If it needs splitting, stage A is a complete, usable server on its own.

### M9 — WebSockets (M)

- Upgrade handshake, framing, fragmentation, control frames, close handshake, keep-alive pings, limits.
- `server.ws`, `@http.ws`, `WebSocket`, `WsMessage`, concurrent-safe send.

Exit: the chat hub; Autobahn passes with no failures in the non-extension cases. **The prototype is complete.**

### M10 — Fibers and poller (L)

- Fiber stacks and context switch; scheduler with work stealing; epoll poller; timers; blocking pool.
- Channels and mutexes re-implemented on fiber parking; collector hook for parked stacks.
- Task-scoped panics with `with` cleanup; handler panics become 500 responses.
- No change to any user program or standard library API.

Exit: every earlier test passes on the new runtime; the connection-count and throughput targets in [07](07-testing-and-performance.md#targets) are met or the gap is explained.

### M11 — Tooling (L, can overlap M10)

- DWARF debug info, readable backtraces, `perf` support.
- `weby fmt`, `weby test` with `@test`.
- Language server: diagnostics, go-to-definition, hover types.
- Project manifest and a minimal dependency mechanism (git URLs first).
- Syntax highlighting grammar for common editors.

### M12 — Performance and ecosystem (ongoing)

- The optimisation backlog in [07](07-testing-and-performance.md#optimisation-backlog).
- HTTP client, PostgreSQL driver, TLS, regular expressions.
- `weby build --race`, the race-detecting build mode that backs up the compile-time sharing checks.
- Header and cookie binding in typed handlers; moving a mutable value into a task.
- Linux aarch64 and macOS ports.
- Collections and router rewritten in Weby.
- Collector decision per [04](04-runtime-and-memory.md#beyond-boehm).

## Gates

Points where the plan is checked against measurements and may change.

| Gate | When | Question | If the answer is bad |
|---|---|---|---|
| 1 | End of M2 | Is allocation-heavy code at least 10× faster than CPython, and is the GC stress mode clean? | Fix before building traits on top; revisit descriptor and allocation paths |
| 2 | End of M8 | On the JSON endpoint benchmark, how far behind Go are we, and where does the profile say the time goes (GC, syscalls, parsing, dispatch)? | If GC dominates: start allocation-reduction work before M10. If the thread model dominates: proceed to M10 as planned |
| 3 | End of M10 | Are we within the Go-parity targets? | Decide between MMTk integration, escape analysis first, or accepting the gap |

## The first vertical slice

If the aim is to see something working as early as possible, the shortest path to a running HTTP endpoint is M0, M1, M2, the trait-object and enum parts of M3, closures from M4, then M8 with responses built from f-strings. Modules, generics, derives, JSON and channels can all be skipped for that demo. It is a reasonable way to de-risk the runtime boundary early, as long as the skipped work is then done in order.

## Working practices

- Every language feature lands with: parser tests, at least one failing-program diagnostic test, and at least one end-to-end run test.
- Every runtime function lands with a test that runs under the GC stress mode.
- Benchmarks are run and recorded at the end of each milestone, so regressions and trends are visible.
- Syntax or semantics changes update [01](01-language-design.md) or [02](02-type-system-and-traits.md) in the same change. Those two documents become the language reference.
- Decisions that reverse something in [08](08-decisions-and-risks.md) are recorded there with the evidence.
