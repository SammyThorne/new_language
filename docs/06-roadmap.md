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

As of 2026-11-21.

| Milestone | State | Notes |
|---|---|---|
| M0 | Done | Workspace, runtime, LLVM object emission, linking, end-to-end test harness. Continuous integration runs the tests, lints and stress programs on GitHub (`.github/workflows/ci.yml`) |
| M1 | Done | Everything listed below, with `float` formatted exactly as CPython does. The first benchmark is recorded in [bench/README.md](../bench/README.md) |
| M2 | Done | Everything listed below. Gate 1 passed. Sets were not in the milestone and are not included |
| M3 | Done | Everything listed below, with two differences. `match` is compiled as a test per case in order, not as a decision tree (D31). `Option` and `Result` behave as enums in `match`, `?` and their methods, but stay built into the compiler until generics exist (M4) |
| M7 | Done | Everything listed below. The conformance corpus is hand-written in the style of JSONTestSuite (67 documents that must parse, 94 that must not), because fetching the real suite is a download (D47); it agrees with CPython's parser except where CPython is knowingly lenient. `json.Value` derives `Eq`, which needed `==` on dicts, now supported |
| M6 | Done | Everything listed below except three items. `frozenset` is not there, because sets are not implemented yet; `tuple[T, ...]` and `frozendict[K, V]` are. `RwLock` (a stretch item) is not there. The runtime has not been run under ThreadSanitizer: that needs a nightly Rust toolchain, which is not installed on the development machine (D45). A panic in any task ends the whole program (D24), so "joining a panicked task panics" holds trivially |
| M5 | Done | Everything listed below, with two differences. `with` works with any type that has an `exit` method (and optionally `enter`), rather than a `Scoped[T]` trait, for the same reason as iteration (D38). The sized numeric types support conversion, comparison, `+`, `-`, `*`, bit operations and printing, but not `//`, `%` or `**` (D41) |
| M12 | Second part done | Ongoing by design. Done: the collector's growth policy (D73), single-lookup dict updates (D74), responses serialised once (D75), regular expressions (D76), an HTTP client (D77), header and cookie binding in typed handlers (D78), TLS for the server, the client and WebSockets (D79), a PostgreSQL driver (D80), the Autobahn and JSONTestSuite runs (D81; 0 failures), and phase 1 of the performance plan in [09](09-performance-plan.md) (D82). Not started: escape analysis (needs the mid-level IR), `--race`, the aarch64 and macOS ports (no machine to test on), moving mutable values into tasks, and collections and the router rewritten in Viper |
| M11 | Done | Everything listed below, with these differences. Debug information is line tables and function names, without variables or types, so a debugger can break, step and show a backtrace but not print locals (D67). The language server re-checks the whole program on each change and offers diagnostics, hover, go to definition and formatting; there is no completion yet (D70). Editor support is a TextMate grammar (VS Code and others) and a Vim syntax file; a VS Code client for the language server is not included, since it needs npm packages (D72) |
| M10 | Done | Everything listed below except three items. There is no blocking pool: file I/O and `@extern` calls run on the worker that makes them. Timers are one timer thread rather than a heap per worker. Stack overflow hits the guard page and ends the process with a segmentation fault, not a clean panic. A panic releases the mutexes its task held, but other `with` exits (closing a file) are not run (D64). No user program or standard library API changed (D61 to D66) |
| M9 | Done | Everything listed below. Autobahn is replaced by a hand-written client test that checks the same protocol cases (D59), since fetching the suite is a download. Binary messages needed a `bytes` type, which is now in the prelude (D57). `ws.request` became accessors on the `WebSocket` (D58) |
| M8 | Done | Everything listed below, with these differences. The HTTP parser is written by hand in the runtime rather than using `httparse`, and the router is written in Viper (in `stdlib/http.lang`) as a list of routes tried in order rather than a radix tree with `matchit`, since adding crates is a download (D49). `server.group`, `use`, `on_error`, graceful shutdown and the `log` module are there. Header and cookie binding stay on `http.Request`, as planned. The Python baseline is the standard library's `ThreadingHTTPServer`, not a framework (D56) |
| M4 | Done | Everything listed below, with three differences. Iteration uses methods named `next` and `iter` rather than generic `Iterable`/`Iterator` traits, because traits cannot have type parameters yet (D34). Methods cannot have their own type parameters; generic functions are top-level (D33). `Option` and `Result` remain built in: they need nothing generics would add |
| API documentation | Done | The [OpenAPI plan](12-openapi-docs-plan.md), five milestones after the 1.0.0 release, released as 1.1.0 (D96 to D99): documentation arguments on route annotations and docstrings kept (M1); OpenAPI 3.1 operations and schemas written at compile time, only for programs that use them (M2, M3); a documentation page written for the project, served from the runtime (M4); security schemes, tags, and the documentation (M5). Left out, with reasons in the plan: call-style routes described beyond their path, generic structs as handler types, OAuth2 flows, file uploads, and a command that writes the document without running the program |
| Google sign-in | Done, apart from a check with a real Google client | The [Google sign-in plan](13-google-oauth-plan.md), six milestones: an `auth` module that signs users in with Google from the console's credentials file, and `@auth.required` and `auth.User` parameters to protect routes. Milestone 1 done (D100): any number of cookies per response, URL and form encoding, `post_form`, test requests with headers and a query string, and values middleware keeps with a request. Milestone 2 done (D101): the `crypto` module (secure random tokens, SHA-256, HMAC-SHA256, constant-time comparison, base64 and base64url, RS256 signature checks). Milestone 3 done (D102): the `auth` module, with the sign-in flow, the session cookie, the policies, `auth.required` and `auth.protect`, tested against a fake Google. Milestone 4 done (D103): ID token signatures checked against Google's keys, cached under a lock for their `max-age`. Milestone 5 done (D104): `@auth.required` and `auth.User` parameters protect mounted routes, documented in the API documentation, with error E0342. Milestone 6 done: the setup guide in docs/05 and on the site, `examples/google_login`, and the release notes. The check with a real Google client awaits the owner's credentials |
| Production web features | Milestones 1 to 13 of 25 done | The [production web plan](14-production-web-plan.md): CORS, static files, compression, dates and UUIDs, configuration, metrics with a dashboard, traces, size limits, uploads, timeouts, rate limits, streaming, templates, SQLite, migrations, jobs, CSRF, and more sign-in providers. Milestone 1 done (D105): `log` fields and JSON lines, `log.with_fields`, buffered log output while serving, `http.request_log()` with request IDs carried by every line of the request, `req.client_ip()` with `server.trusted_proxies`, `req.body()` as bytes, `http.binary`, and `os.setenv`. Milestone 2 (D106): `http.cors`. Milestone 3 (D107): `server.static`, with validators, ranges, pre-compressed files and a single-page-app fallback. Milestone 4 (D108): `http.compress`, and gzip in the client. Milestone 5 (D110): `datetime`, with time zones, durations, JSON, OpenAPI and `postgres` columns. Milestone 6 (D111): `uuid`, v4 and v7, as path and query parameters, and as request IDs. Milestone 7 (D112): `config`, with `.env` files, typed settings that report every problem at once, and `config.Secret`. Milestone 8 (D113): `server.metrics()` in the Prometheus format with Basic authentication, the `metrics` module, `server.health()` with readiness checks, and `drain_delay`. Milestone 9 (D114): the metrics dashboard, `METRICS_PASSWORD_HASH` with `viper hash-password`, and a lock-out after failed sign-ins. Milestone 10 (D115): the `trace` module, with OTLP export, `traceparent` in and out, spans of requests, fetches and queries, and trace IDs in log lines. Milestone 11 (D116): bodies over 64 KiB read on demand, `max_body` per route and group, `100-continue` on demand, draining, and limits on headers and URLs. Milestone 12 (D117): `req.form()`, `http.Form[T]` and `http.Upload` parameters, files kept on disk past 1 MiB, form limits, and uploads from the documentation page. Milestone 13 (D118): `timeout` on routes, groups and servers, cancellation at every wait when a deadline passes or the client hangs up, `connect_timeout` in the client, `req.deadline()`, `req.cancelled()` and `http.check_cancelled()` |
| Release polish | Done | The [release polish plan](11-release-polish-plan.md), five milestones before the first release. Milestone 1 done (D91): one-line blocks, raw strings, `del`, constant expressions, `!r` and `!s` in f-strings, `print` keywords, `match` expressions, and better help messages. Milestone 2 done (D92): about 30 methods of `str`, `list` and `dict`, and `ord`, `chr`, `reversed` and `list(...)`, written in the prelude. Milestone 3 done (D93): `set[T]` with literals, comprehensions, operators and JSON. Milestone 4 done (D94): methods with their own type parameters, and `case Circle(c):` on trait objects. Milestone 5 done (D95): `panic: stack overflow` instead of a segmentation fault, completion in the language server, an unused import warning, the error reference and `viper explain`, and executables a fraction of the size. Left out, with reasons in D91 to D95: bytes literals, `frozenset`, preemption, and empty collections typed by their first use |

Exit programs, all in `tests/run/`: `fizzbuzz.vi`, `primes.vi` and `fib.vi` (M1), `word_frequency.vi` (M2), `shapes.vi` and `evaluator.vi` (M3), and `generics.vi`, `closures.vi` and `iterators.vi` (M4), and `calculator.vi` (the M3 evaluator split into `calc/syntax.vi`, `calc/parser.vi` and `calc/eval.vi`) and `file_stats.vi` (a file-reading command-line tool) (M5), `worker_pool.vi` and `shared_counter.vi` (M6), and `json_roundtrip.vi`, `json_dynamic.vi` and `json_conformance.vi` (M7). The M8 exit programs are `tests/http/todo_annotations.vi` and `tests/http/todo_calls.vi`, the same todo service in the two styles; `crates/lang_cli/tests/http.rs` starts each, sends both the same script of requests over a raw socket (routing, binding errors, keep-alive, pipelining, chunked bodies, `100-continue`, `HEAD`, malformed requests, concurrent clients), and stops it with SIGTERM. The M9 exit program is `tests/http/chat.vi`, the chat hub plus an echo endpoint; `crates/lang_cli/tests/websocket.rs` checks the hub's joins, broadcasts and leaves, and the protocol cases from Autobahn's sections 1 to 7 and 9, with forty concurrent clients and a graceful shutdown. It also passes with the collector running before every allocation. M10's exit criterion is that every earlier test passes on the new runtime, which it does, unchanged, in debug, release and collector-stress modes; `tests/stress.sh` again ran a thousand times without a failure. M12 added `tests/run/dict_update.vi`, `tests/run/regex.vi` and `tests/run/http_client.vi` (a server and client in one program, with header and cookie binding), `tests/run/https.vi` (the same over TLS, with a certificate refused for the wrong name), `crates/lang_cli/tests/tls.rs` (HTTPS keep-alive, large bodies and secure WebSockets driven by a `rustls` client), `tests/db/database.vi` with `crates/lang_cli/tests/postgres.rs` (the PostgreSQL client against a real server, when one is configured: see `tests/db/README.md`), and the conformance programs in `tests/conformance/`. M11's commands are tested end to end in `crates/lang_cli/tests/tooling.rs` (`test` with passing and failing tests, `fmt --check`, and a project with a git dependency in a local repository and a path dependency, including the lock file and `fetch --update`), the language server in a scripted session in `crates/lang_cli/src/lsp.rs`, and the formatter against every source file in the repository (it must keep each file's tokens and be idempotent). M10 added `tests/run/task_panic.vi` (a panicking task releases its mutex and the others carry on), a panicking route in both todo services (a 500, and the server keeps serving), and `bench/ws_idle/` for the idle-connection target. The programs that must not compile are `tests/ui/share_*.vi`, `lock_guard_escape.vi` and `mutex_contents.vi`. `tests/stress.sh` runs the two concurrency programs a thousand times in release mode and a hundred times under GC stress; at the end of M6 it passed with no failures. The binary-trees and sort benchmarks are in `bench/`.

**Gate 1 result.** Allocation-heavy code (binary trees, depth 18) runs 20x faster than CPython 3.13, against a target of 10x. All 20 test programs pass in the GC stress mode.

**Against Go 1.27.4**, every benchmark is already inside the end-of-prototype targets in [07](07-testing-and-performance.md#targets). Viper takes 1.28x Go's time on allocation, 1.46x and 1.51x on collections and strings, and is faster than Go on recursive calls. [bench/README.md](../bench/README.md) has the numbers and the reasons for each gap.

Choices made during M1 to M12 that the earlier documents did not settle are recorded in [08](08-decisions-and-risks.md) as D21 to D82.

**M12 so far, and the Gate 3 gaps.** Both gaps Gate 3 named have narrowed. `word_count` went from 1.47x Go's time to 1.15x *faster* than Go, from compiling the dict-update idiom to one lookup. `/items` went from 0.42x to 0.72x of Go's throughput at 256 connections (114,000 to 219,000 requests per second), from the collector's growth policy and serialising responses once; small responses are now 1.5x faster than Go. `binary_trees` is at 1.11x Go's time, at the cost of more memory (72 MB against 57 MB). `spawn` measured 3.26x Go's time in the latest run, just outside its 3x target; Go's time varies more between runs than Viper's. Escape analysis remains the next step for allocation-heavy handlers ([bench/README.md](../bench/README.md)).

**M10 result and Gate 3.** Against the "after fibers and tuning" column of [07](07-testing-and-performance.md#targets) ([bench/README.md](../bench/README.md)):

| Target | Result | Met |
|---|---|---|
| CPU within 1.1x of Go | `fib` is 1.95x faster | Yes |
| Allocation within 1.5x | `binary_trees` 1.29x | Yes |
| Collections and strings within 1.3x | `sieve` 1.26x, `word_count` 1.54x | `word_count` no |
| Typed JSON faster than `encoding/json` | 1.10x faster | Yes |
| HTTP throughput at 256 connections within 1.3x | Small responses 1.13x to 1.24x faster; 1 KB JSON lists of 20 records 0.42x | Large responses no |
| HTTP p99 latency within 1.5x | Lower than Go's on every endpoint | Yes |
| 50,000 idle WebSockets under 16 KiB each | 11.3 KiB each | Yes |
| Spawn and join within 3x of goroutines | 2.7x | Yes |

Two gaps remain, and both are allocation. `word_count` is string-keyed dict work, which the M2 notes already put on the backlog (inlinable collections, an entry API). `/items` allocates 20 records (each with a formatted string and a list) and a 1.1 KB string per request; collection is about a third of its time, and Viper's median latency there is five times Go's. The gate's options are MMTk, escape analysis first, or accepting the gap. The recommendation is **escape analysis and allocation reduction first** (backlog items 3, 5 and 8: buffer reuse in the HTTP path, stack-allocating values that do not escape, sizing f-strings once), because they reduce the work any collector has to do and need the mid-level IR that M12 plans anyway; a precise collector is reconsidered with the measurements after that.

**The prototype is complete** (M0 to M9): every exit program from the summary table runs and is in the test suite.

**M8 result and Gate 2.** With 64 connections, Viper serves 734,000 plain-text and 717,000 small-JSON requests per second, against Go's 565,000 and 531,000; on a 1.1 KB JSON list of 20 records it serves 134,000 against Go's 263,000. CPython's standard-library server manages 16,000 to 24,000 ([bench/README.md](../bench/README.md)). On the large-JSON endpoint, giving the collector a large initial heap recovers 50%, so collection is about a third of the time; most of the rest is 64 threads contending for Boehm's allocator. That is the thread model more than the collector, so the plan proceeds to M10, whose one worker thread per core should reduce the contention; Gate 3 measures it again. Allocation reduction in the request path stays on the M12 backlog.

**M7 result.** Typed JSON (encode and decode of 100,000 line items, ten times) takes 0.52 s, against 0.54 s for Go's `encoding/json` and 3.23 s for CPython ([bench/README.md](../bench/README.md)).

**M4 result.** Sorting a million records with a key closure takes 0.74 s, against 1.12 s for Go's `sort.SliceStable` with a comparator closure and 2.05 s for CPython ([bench/README.md](../bench/README.md)).

**M3 review.** The roadmap suggests reviewing the syntax at this point. One gap showed up and was fixed: a trait whose methods all have defaults could not be implemented, because a block cannot be empty. `pass` was added for that (D29).

## Milestones in detail

### M0 — Skeleton and toolchain (S)

- Cargo workspace with the crates from [03](03-compiler-architecture.md#repository-layout).
- `inkwell` building against the installed LLVM 19; confirm the feature flag and record the exact versions.
- `lang_rt` as a static library with one function (print a string) and a C-ABI entry that calls the program's `main`.
- `viper build` that constructs a trivial LLVM module by hand, emits an object, and links it.
- End-to-end test harness: compile a `.vi` file, run it, compare standard output and exit code against expectations embedded in comments.
- Continuous integration running build, tests, formatting and lints.

Exit: `viper run hello.vi` prints a line. The source file is not parsed yet; the point is that every stage of the toolchain is connected.

### M1 — Core language (L)

- Lexer with the full layout algorithm; parser for functions, statements and expressions; diagnostics rendering.
- Types: `int`, `float`, `bool`, and `str` literals that can be printed.
- Variables by assignment, definite-assignment analysis, `if`/`elif`/`else`, `while`, `for` over `range`, `break`, `continue`, `return`.
- Functions with typed parameters, recursion, forward references.
- Type checker for the above; HIR; codegen; panics with source locations; debug and release modes.
- `viper check` and the `--emit` dumps.
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
- `viper fmt`, `viper test` with `@test`.
- Language server: diagnostics, go-to-definition, hover types.
- Project manifest and a minimal dependency mechanism (git URLs first).
- Syntax highlighting grammar for common editors.

### M12 — Performance and ecosystem (ongoing)

- The optimisation backlog in [07](07-testing-and-performance.md#optimisation-backlog).
- HTTP client, PostgreSQL driver, TLS, regular expressions.
- `viper build --race`, the race-detecting build mode that backs up the compile-time sharing checks.
- Header and cookie binding in typed handlers; moving a mutable value into a task.
- Linux aarch64 and macOS ports.
- Collections and router rewritten in Viper.
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
