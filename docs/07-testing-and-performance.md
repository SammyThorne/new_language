# 07 — Testing and performance

## Testing

### Layers

| Layer | What it checks | How |
|---|---|---|
| Lexer | Token streams, especially layout tokens and f-strings | Snapshot tests: source in, token dump out |
| Parser | AST shape, precedence, error recovery | Snapshot tests of an AST dump |
| Diagnostics | Message text, spans, codes | "UI" tests: a `.wb` file that must fail, with the rendered error stored next to it |
| Type checker | Accepts valid programs, rejects invalid ones | UI tests plus HIR snapshots for desugaring |
| End to end | The whole pipeline and runtime | `.wb` programs with expected standard output, exit code, or panic message in header comments. Run in debug and release modes |
| Runtime | Strings, collections, channels, HTTP parsing, WebSocket framing | Ordinary Rust unit tests inside `lang_rt` |
| Services | HTTP and WebSocket behaviour | Start the compiled server on an ephemeral port and drive it with a scripted client |
| Benchmarks | Performance does not regress | Recorded per milestone; later checked automatically with a tolerance |

The end-to-end layer carries the most weight. A language feature is not done until a program using it runs.

### Robustness

| Technique | Target | When |
|---|---|---|
| GC stress mode (collect on every allocation) | Missing roots, violations of the Rust heap rule | From M2, whole end-to-end suite |
| LLVM module verifier | Malformed IR from codegen | Always on in compiler debug builds and CI |
| AddressSanitizer on the runtime and generated code | Out-of-bounds and use-after-free in native code | CI job from M2 |
| ThreadSanitizer on the runtime | Races in channels, scheduler, poller | CI job from M6 |
| "Must not compile" programs | The sharing, visibility, frozen and route-signature checks reject what they should, with the expected message | From the milestone that adds each check |
| Race-detecting build mode (`weby build --race`) | Races in user programs that the compile-time rules cannot see | Post-prototype |
| Fuzzing the lexer and parser (`cargo-fuzz`) | Compiler crashes on arbitrary input | From M3; the property is "never panics" |
| Fuzzing the HTTP parser, WebSocket framer and JSON scanner | Crashes and hangs on hostile input | From M7–M9; these face the network |
| Differential testing against CPython | Arithmetic, string methods and formatting that are meant to match Python | Small generated programs, from M2 |

### Protocol conformance

- **HTTP.** A scripted suite covering keep-alive, pipelining, chunked bodies, oversized headers and bodies, malformed request lines, slow clients and timeouts.
- **WebSocket.** The Autobahn test suite, the standard conformance suite for RFC 6455 implementations. Target: no failures outside the compression extension cases.
- **JSON.** A public conformance corpus of must-accept and must-reject documents, plus round-trip tests of floats.

### Continuous integration

On every change: build, compiler unit and snapshot tests, end-to-end suite in debug and release, GC stress run, formatting and lints. Nightly: sanitizer jobs, fuzzing for a fixed time budget, benchmark run with results archived.

## Performance

### Where the speed comes from

| Source | Mechanism |
|---|---|
| Static types | Field access is a fixed offset; arithmetic is machine arithmetic; no boxing of scalars; no dictionary lookups for attributes |
| LLVM | Inlining, loop optimisation, vectorisation, the same optimiser as Clang and Rust |
| Monomorphised generics | Collections specialised per element type; no boxing in `list[int]` |
| Whole-program compilation | Cross-module inlining, dead code removal, devirtualisation |
| Compiled JSON | Per-type encoders and decoders, no reflection |
| Native protocol code | HTTP parsing and WebSocket framing in optimised Rust |
| Parallelism | Real multi-core execution with no global interpreter lock |

### Where the costs are

| Cost | Size | Plan |
|---|---|---|
| Garbage collection (Boehm) | The largest known gap to Go on allocation-heavy code | Pointer-free allocation, fewer allocations, then precise GC ([04](04-runtime-and-memory.md#beyond-boehm)) |
| Every struct on the heap | Significant for small short-lived values | Escape analysis; optional value structs |
| Thread-per-task (stage 1) | Dominant at high connection counts | Fibers (M10) |
| Runtime calls not inlined | Small per call, adds up in collection-heavy loops | Inline fast paths; move collections into Weby |
| Bounds checks | Usually a few percent | LLVM removes many; iteration emits none |
| Dynamic dispatch | A few nanoseconds per call, blocks inlining | Prefer generics in hot paths; devirtualise when the type is known |
| Object header | Eight bytes per object | Accepted |

### Benchmark suite

Each benchmark exists in Weby, Go and Python, doing the same work the idiomatic way in each language. Programs live in `bench/`.

| Group | Benchmarks | What it isolates |
|---|---|---|
| CPU | Recursive Fibonacci, n-body, spectral norm, prime sieve | Codegen quality, call overhead, float and integer loops |
| Allocation | Binary trees, building and discarding many small structs | Allocator and collector |
| Collections | Word frequency, dict insert and lookup, list sort | Runtime collections, hashing, string handling |
| Strings | Concatenation, formatting, split and join | String runtime, f-strings |
| Dispatch | A loop over trait objects versus the same loop with generics | Vtable cost, devirtualisation |
| JSON | Encode and decode a small object, a large array of objects, and an untyped document | Derived code and scanner |
| HTTP | Plain-text response; JSON response; JSON request and response; a handler that waits 10 ms (simulated database) | Server stack end to end |
| WebSocket | Echo round-trip latency; broadcast to N clients; N idle connections (memory) | Framing, scheduler, per-connection cost |
| Tasks | Spawn and join 100,000 tasks; ping-pong on a channel | Scheduler |

Baselines: CPython 3.13 (with a mainstream ASGI framework and server for the HTTP rows) and the current stable Go release with `net/http` and `encoding/json`. Go is not installed on the development machine yet and is needed from M1 for baselines.

### Method

- Fixed machine, quiet system, CPU governor set to performance, results recorded with hardware and tool versions.
- CLI benchmarks with `hyperfine` (warm-up runs, multiple samples, report mean and spread).
- HTTP benchmarks with `wrk` or `oha` from the same host at several connection counts (16, 256, 4096), reporting requests per second and latency percentiles (p50, p99), not only throughput.
- Memory: peak resident set size for every benchmark.
- Results are committed as data files per milestone so trends can be plotted.
- Profiling with `perf` and flame graphs. Useful only once debug info exists, which is why it is the first tooling task after the prototype.

### Targets

Targets are ratios against the baselines on the same machine. They are goals to steer by, not predictions.

| Benchmark group | By end of prototype (M9) | After fibers and tuning (M10–M12) |
|---|---|---|
| CPU | Within 1.3× of Go; at least 30× faster than CPython | Within 1.1× of Go |
| Allocation | Within 3× of Go; at least 10× faster than CPython | Within 1.5× of Go |
| Collections and strings | Within 2× of Go | Within 1.3× of Go |
| JSON typed | Within 1.5× of Go's `encoding/json` | Faster than `encoding/json` |
| HTTP throughput at 256 connections | At least 5× a Python ASGI stack on one worker; within 2× of Go | Within 1.3× of Go |
| HTTP p99 latency at 256 connections | Within 3× of Go | Within 1.5× of Go |
| Idle WebSocket connections | 5,000 | 50,000 at under 16 KiB each |
| Task spawn and join | Not a target (threads) | Within 3× of goroutines |

"Faster than Python interpreted" is expected to be met at M1 and never be in doubt afterwards. "Same speed as Go" is the M10–M12 column and is realistic for CPU, JSON and HTTP; allocation-heavy workloads are where a gap is most likely to remain until the collector is replaced.

### Optimisation backlog

Ordered by expected payoff for web workloads. None of this is prototype work except the first two items, which are design choices made up front.

1. Pointer-free allocation for byte data (in the M2 design).
2. Derived JSON without intermediate trees (in the M7 design).
3. Per-connection buffer reuse and lazy header materialisation in the HTTP path.
4. Fibers and epoll (M10).
5. Escape analysis: stack-allocate structs, closures and small lists that do not outlive their function. Requires the mid-level IR.
6. Devirtualisation: when a trait object's concrete type is known at the call site, call directly; speculative devirtualisation for traits with one implementor in the program.
7. Small-string optimisation: strings up to 15 bytes stored inline in the (pointer, length) pair.
8. String builder elision: f-strings and concatenation chains sized once and written once.
9. Collections in Weby: monomorphised `dict` with an open-addressing table, so hashing and comparison inline.
10. Loop yield checks that cost one predictable branch.
11. ThinLTO across the runtime boundary, if LLVM versions of `rustc` and the compiler can be aligned.
12. Profile-guided optimisation as a build option.
13. A precise, generational collector.
