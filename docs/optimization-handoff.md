# Optimisation handoff: state of the project for deep planning

This document is the starting point for planning the next round of performance work. It is meant to be read by someone (or an agent) with no other context, and it says where to look for detail. State as of 2026-11-19, commit `7b7c95f`.

> **Since then:** the [final optimisation plan](10-final-optimization-plan.md) has been carried out (decision records D87 to D90). Its results, and what is left, are in [section 11](#11-after-the-final-optimisation-pass) at the end. The sections in between describe the state before it and are kept as the record of what the plan started from.

## 1. What the project is

Viper is a compiled, Python-shaped language for web backends.
- **Language.** Indentation syntax, structs and traits instead of classes, generics by monomorphisation.
- **Standard library.** HTTP/1.1 server and client, TLS, WebSockets, JSON, PostgreSQL, regular expressions, tasks and channels.
- **Compilation.** Source is compiled through LLVM 19 to a native executable linked with a Rust runtime (`liblang_rt.a`) and the Boehm conservative garbage collector (`libgc.so.1`).
- **Status.** The prototype milestones (0 to 11) are done. Milestone 12 ("performance and ecosystem") is ongoing. The five phases of the performance plan ([09-performance-plan.md](09-performance-plan.md)) have all been carried out. Phase 5 ended in a decision, not a change of collector.

**Headline:**
- Every benchmark in `bench/` and every HTTP endpoint measured is at or ahead of Go 1.27 on the benchmark machine.
- Batch programs that allocate heavily use up to 1.8x Go's memory.
- The user-facing summary is [web-performance.md](web-performance.md).

## 2. Where the numbers stand

Latest full runs (phase 5, 2026-11-19), from [bench/README.md](../bench/README.md). The machine varies by about 10% between runs, so compare the ratios.

| Benchmark | Viper | Go | Viper vs Go | Memory Viper / Go | What it stresses |
|---|---|---|---|---|---|
| binary_trees | 0.736 s | 0.938 s | 1.27x faster | 65 / 40 MB | Allocation, collection |
| dispatch | 0.011 s | 0.032 s | 3.05x faster | 13 / 13 MB | Trait-object calls |
| fib | 0.021 s | 0.038 s | 1.81x faster | 13 / 13 MB | Calls, integers |
| json_orders | 0.606 s | 0.664 s | 1.10x faster | 71 / 59 MB | Typed JSON encode and decode |
| sieve | 0.061 s | 0.064 s | 1.05x faster | 60 / 55 MB | List indexing, append |
| sort | 0.537 s | 1.203 s | 2.24x faster | 149 / 81 MB | Sort with key closure, tuple keys |
| spawn | 0.047 s | 0.047 s | 1.00x | 15 / 14 MB | Task creation and join |
| word_count | 0.674 s | 0.825 s | 1.22x faster | 13 / 13 MB | split, lower, str-keyed dict |

HTTP: requests per second, 256 keep-alive connections, closed-loop load generator. Server on 6 logical CPUs (3 cores with SMT); the load generator on the other 6.

| Endpoint | Viper (p50 / p99 ms) | Go (p50 / p99 ms) | Viper vs Go | Per-request CPU on one core, Viper / Go |
|---|---|---|---|---|
| `/plaintext` | 1,010,397 (0.21 / 0.79) | 528,241 (0.36 / 1.77) | 1.91x | 2.6 / 5.4 µs |
| `/json` | 901,450 (0.23 / 0.98) | 495,903 (0.38 / 1.93) | 1.82x | 2.7 / 5.6 µs |
| `/items` (20 records, 1.1 KB JSON) | 229,082 (0.99 / 2.46) | 227,851 (0.78 / 5.48) | 1.01x | 9.6 / 10.9 µs |
| `/users/{id}` (behind 10 routes) | 848,079 (0.25 / 0.92) | 475,075 (0.40 / 1.90) | 1.79x | 3.2 / 5.9 µs |
| `POST /orders` (JSON decode to structs) | 615,022 (0.36 / 1.04) | 289,549 (0.65 / 3.60) | 2.12x | 4.6 / 9.1 µs |

**Other measures:**

| Measure | Viper | Go |
|---|---|---|
| Idle server memory | 4.3 MB | 8.9 MB |
| Peak memory under load | 20 to 28 MB | 22 to 25 MB |
| Idle WebSocket connection | 15.5 KiB, measured with 50,000 | not measured |
| Start-up | 3 ms | 4 ms |
| Executable | 7.6 MB | 9.8 MB |

**`/items` is the weakest HTTP row.** It ranged from 1.01x to 1.13x across runs, and its median latency is higher than Go's (0.99 against 0.78 ms) although its p99 is lower.

## 3. Architecture, as it bears on performance

Detail: [03-compiler-architecture.md](03-compiler-architecture.md), [04-runtime-and-memory.md](04-runtime-and-memory.md).

### Compiler (`crates/`)

| Crate | Lines | Role |
|---|---|---|
| `lang_syntax` | 3.7k | Lexer (layout-sensitive), parser, AST |
| `lang_sema` | 12.9k | Module graph, resolution, type checking, typed HIR, derivations (`@derive(Json)` etc.), monomorphisation |
| `lang_codegen` | 6.2k | HIR to LLVM IR via `inkwell` 0.11 (LLVM 19). Includes the escape analysis (`escape.rs`), the allocation lowering (`alloc.rs`) and the trait-call dispatchers (`traits.rs`) |
| `lang_rt` | 8.8k | Runtime in Rust: collector glue, strings, lists, dicts, builders, JSON, fibers and scheduler, poller, HTTP server and client, WebSockets, TLS (`rustls`), PostgreSQL |
| `lang_cli` | 2.0k | The `viper` driver: build, run, test, fmt, check, and the language server |
| `lang_format`, `lang_diag`, `lang_names` | small | Number formatting, diagnostics, the language's name in one place |

**Pipeline:**
1. Parse, resolve and type-check into a typed HIR.
2. Monomorphise from `main`.
3. Generate LLVM IR straight from the HIR. There is no mid-level IR (no CFG IR, no SSA of its own).
4. Run LLVM's `default<O2>`.
5. Lower allocation calls to inline free-list pops, then run `always-inline`.
6. Emit an object file and link with `cc`, `liblang_rt.a` and `libgc`.

**Key code-generation facts:**
- **Types.**
  - Structs are heap objects: `ptr` to `{ descriptor ptr, fields... }`, so every struct has an 8-byte header that the runtime barely reads.
  - `str` is `{ptr, len}`, immutable, and shares bytes for substrings.
  - Lists and dicts are pointers to runtime layouts. Generated code reads the list header (`len`, `cap`, `data`) and the dict `len` directly.
  - Trait objects are `{data, vtable}`; function values are `{code, env}`.
- **Calling convention.** Internal functions are `fastcc` and internal linkage. Small aggregates are passed as scalars.
- **Allocation.**
  - Everything is emitted as `lrt_alloc_raw(size)`, declared to LLVM as an allocator (`allockind`, `allocsize`, `noalias`). LLVM deletes allocations it can see through.
  - After `O2`, constant-size calls of up to 255 bytes become an inline pop from a per-thread free list (refilled by `GC_malloc_many`). The list is read through inline assembly, because tasks migrate between threads.
  - Structs with no reference fields use `GC_malloc_atomic`.
- **Escape analysis** (structs only). A struct construction whose object provably does not outlive its call becomes an `alloca`. Whole-program parameter summaries are iterated to a fixpoint.
- **Alias information.** TBAA on list and dict headers and struct fields. `append`, list indexing and `len` are inlined.
- **Trait-object calls** go through an always-inlined `dispatch.Trait.method`. With at most four implementing types, it compares vtables and calls directly; otherwise it makes the indirect call.
- **Strings.** f-strings use a stack builder sized from an estimate, and `a + b + c` chains allocate once.
- **`d[k] = d.get_or(k, x) + y`** does one lookup.

### Runtime (`crates/lang_rt`)

- **Collector.** Boehm 8.2.8 from the system, used conservatively, with all interior pointers recognised.
  - Interior pointers make Boehm add 1 byte to every object, and it rounds to 16-byte granules.
  - Settings: free-space divisor 2; at least 4 MiB allocated between collections (`VIPER_GC_MIN_ALLOC`); multi-threaded mode starts at the first `spawn`.
  - `VIPER_GC_STRESS` collects before every allocation and is used in tests.
- **Fibers** (`fiber.rs`). M:N, one worker thread per core. 1 MiB `mmap`ed stacks, committed on touch, with the fiber record at the top of its own stack.
  - Each worker has a mutex-protected run queue with an atomic length.
  - There is a lock-free global queue (Vyukov) for wakeups from threads that are not workers, and work-stealing of half a queue.
  - Idle workers sleep in `epoll_wait` on one shared poller.
  - The collector scans fiber stacks through Boehm's "push other roots" hook and its sp corrector. There is no lock on the context switch.
- **The program's `main` runs on the main thread, which is not a worker.**
- **HTTP server** (`http.rs`).
  - One fiber per connection. The request is parsed into a Rust struct, and the compiled handler is called with an integer handle.
  - The handler reads the request through `lrt_http_req_*` calls and leaves a response with `lrt_http_respond`, which serialises it into a `Vec<u8>` that the connection writes.
  - Routing, middleware and the `Request`/`Response` types are written in Viper (`stdlib/http.lang`). Fixed paths are answered from a precomputed table, and parameterised routes are matched without allocating.
- **JSON.** `@derive(Json)` generates encoders and decoders that work directly on bytes, without intermediate trees (`lang_codegen/src/json.rs`, `lang_rt/src/json.rs`, `builder.rs`).
- **Dicts.** Insertion-ordered, open addressing with linear probing, and an index array of `i64` entry numbers. Hash and equality come through function pointers in the type descriptor, except for `str` keys, which the runtime handles directly.

## 4. What has been done, and what was learned

Each phase has a decision record in [08-decisions-and-risks.md](08-decisions-and-risks.md) with its numbers.

| Phase | Record | Done | Main effects |
|---|---|---|---|
| 1. Tell LLVM what it needs | D82 | Allocator attributes, memory attributes on string functions, TBAA, inline `append`/index/`len`/string equality, builders on the stack | `sieve` 1.2x Go's time to faster; `sort` 1.6x to 2.4x faster than Go |
| 2. Allocate less | D83 | Inline allocation fast path, pointer-free structs, escape analysis for structs | `binary_trees` 0.97 s to 0.64 s (faster than Go) |
| 3. Runtime | D84 | No collector lock on the switch; cheaper `spawn` (record on stack, lock-free global queue, per-thread stack caches); allocation-free router; runtime-written headers; 4 MiB collection floor | `spawn` 3.1x Go's time to parity; `/items` 0.93x to 1.08x; `/users/{id}` +60% |
| 4. Dispatch and collections | D85 | Devirtualised trait calls, `str`-key dict fast path, presized `split`, ASCII case checks, one-allocation concatenation | `dispatch` 1.7x Go's time to 3.4x faster; `word_count` 1.07x to 1.22x faster |
| 5. Collector | D86 | Measured, then decided to stay on Boehm; divisor 1 to 2 | `binary_trees` 90 to 65 MB |

**Lessons that should shape further planning:**
- **Profile before guessing.** Phase 3's main hypothesis, that the collector's lock on the context switch limited small responses, was wrong: removing it gained about 2%. The real `/items` limiter was the collector running about 1,100 times a second because the live heap was tiny; the fix was a floor on allocation between collections.
- **Collection is not the bottleneck any more.** It is about 2% of `/items` CPU. `libgc` is about 20% of that endpoint's CPU, but almost all of it is allocation from runtime code (`GC_malloc` from Rust, `GC_malloc_many` refills, lazy sweeping), not marking.
- **Small changes move HTTP numbers by 2% to 5% for layout reasons.** Moving the fiber record by a per-stack offset made HTTP slower with no plausible mechanism. Only A/B runs interleaved in alternating order, on pinned CPUs, are trustworthy for small effects. Runs a few minutes apart differ by up to 10%.
- **Spinning workers hurt here.** The main thread is not a worker, so spinning workers take its CPU (`spawn` got 50% slower). Limited to one spinner, it changed nothing.
- **The memory gap is layout, not collection** (D86). `binary_trees` nodes take 32 bytes against Go's 16: the header, Boehm's extra byte and 16-byte granules. `sort` boxes each struct in a list, where Go stores it inline. A precise collector (MMTk Immix) would bring nodes to 24 bytes, not 16, and would not unbox list elements.
- **The obvious single-lookup and builder optimisations already exist.** Check before proposing them again: `dict_update` in `stmt.rs`, D82 builders.

## 5. Where the time goes now

Profiles from `bench/profile/` (a SIGPROF sampler, since `perf` is unavailable). Percentages are of all samples in the process.

**`/items` under load at 256 connections, server on 6 logical CPUs:**

| Share | What |
|---|---|
| 23% | The libc system-call stub: socket `read`/`write` and `epoll_wait` |
| about 20% | `libgc`, mostly allocation; collection itself is about 2% |
| about 10% | glibc `malloc`/`free` and its arena locks (`__lll_lock_wait_private`). This is Rust-heap allocation in the runtime, freed on other threads as fibers migrate |
| about 9% | Float formatting: `float_repr` uses `format!`, so each float allocates Rust `String`s, plus `core::fmt` |
| about 6% | JSON building: `lrt_builder_push`, `push_json_string`, `lrt_builder_push_json_str` |
| about 3% | `memcpy`/`memmove` |
| the rest | Spread over the handler, runtime and scheduler |

**`/plaintext` and the small endpoints:** about half the CPU is system calls. One `read` and one `write` per request per connection, plus `epoll_wait`.

**`word_count`:** dict slot lookup and probe about 50% (string hash inlined, linear probing over `i64` index slots), `split` about 15%, lower-casing about 6%.

**Heap allocations per request in the runtime's HTTP path** (Rust heap, not the collector):
- the method, path, query and remote address as `String`s, the remote copied again for every request;
- each header as two `String`s, with the name lower-cased;
- the response `Vec<u8>`;
- the `Date` string, cloned under a global `Mutex` for every response (`http_date`).

Then the compiled handler copies the method and path again into collected strings. A browser-like request with 10 to 15 headers would make about 30 Rust allocations before the handler runs. The benchmark sends only `Host`, so it understates this.

## 6. Known gaps and leads, with evidence

Ordered roughly by expected value for web workloads. None is started. **The [final optimisation plan](10-final-optimization-plan.md) turns leads 1 to 4, 6 and 7 into milestones, with fresh measurements, and says why the others are left out.**

1. **The HTTP request and response path without Rust heap allocation.**
   - Parse into offsets over the connection's buffer, and keep the remote address once per connection.
   - Cache the date per thread (or in an atomic once a second), and write the status line and headers into a reused per-connection buffer.
   - Expected: removes the 10% `malloc`/arena-lock share on `/items` and more on header-heavy real traffic.
   - Files: `lang_rt/src/http.rs` (`read_request`, `response_bytes`, `http_date`, `Request`).
2. **Float formatting without allocation.** A shortest round-trip algorithm (Ryu or Grisu with fallback) writing straight into the builder. `lang_format::float_repr` currently builds `String`s through `format!`. About 9% of `/items`, and every JSON float.
3. **System calls per request.** One `read` and one `write` per request, `epoll` re-arming, and wake-ups through an `eventfd`.
   - Options: `writev`/batched writes for pipelined requests; edge-triggered `epoll` with read-until-`EAGAIN`; `io_uring` (kernel 6.12 is available, but it is a large change).
   - Half of small-response CPU is here, but Go pays the same costs and is already 2x slower, so this is about absolute capacity, not parity.
4. **Allocation cost from runtime code.** Runtime allocations (`gc::alloc` to `GC_malloc`) do not use the inline free lists that generated code uses. Routing runtime allocations of small constant sizes through the same per-thread cache (`lrt_alloc_slow` / `AllocCache` in `gc.rs`) could cut a large part of `libgc`'s 20%.
5. **Object layout and memory (batch programs at up to 1.8x Go).**
   - (a) **Value types in collections.** Store small, immutable (`@frozen`) or never-aliased structs inline in `list` buffers. This is the only thing that closes `sort`'s gap, and it is a language and code-generation design question about aliasing semantics.
   - (b) **Dropping the struct header.** It is kept for a future precise collector (D86). It would save 16 bytes on structs whose payload is an odd number of words; it would not help `binary_trees`.
   - (c) **The precise-collector path** ([04](04-runtime-and-memory.md#beyond-boehm)): stack maps (statepoints or a shadow stack), then MMTk. It would allow bump allocation, generations and compaction. It is weeks of work, and MMTk is an external crate, which is a download needing the owner's approval.
6. **Idle WebSocket memory regressed** from 11.3 KiB per connection at milestone 10 to 16.5 KiB by phase 2; it is 15.5 KiB now. Not investigated. Likely suspects: stack depth of the WebSocket reader loop touching an extra page, per-connection buffers, the TLS-capable socket wrapper. `bench/ws_idle/` reproduces it.
7. **Dicts beyond `str` keys.**
   - Integer and other keys still hash and compare through function pointers.
   - The index array is `i64` per slot. 4-byte slots with a few hash bits (the plan's phase 4, item 3) would cut cache misses.
   - Code-generated probe loops are not done.
8. **Hot helpers in C compiled to LLVM bitcode** with `clang-19` (installed) and linked into each program's module before optimisation, so they inline into generated code (phase 4, item 2, not done). This works around the version mismatch: `rustc` uses LLVM 23 and the compiler LLVM 19, so the Rust runtime cannot be linked-time optimised into programs.
9. **Escape analysis for closures and list literals.** Only structs today (D83).
10. **Small-string optimisation.** Strings up to 15 bytes inline in the `{ptr, len}` pair. This is backlog item 7 in [07](07-testing-and-performance.md); it touches every string operation.
11. **`main` is not a fiber.** A program whose `main` spawns heavily competes with the workers for CPUs. Running `main` as a fiber would let the scheduler see it.

## 7. Measurement infrastructure and its gaps

**Available:**
- `bench/run.py`, the eight programs against Go and CPython, with outputs checked equal.
- `bench/http/run.py`, the HTTP comparison with pinned CPUs. `bench/http/one_core.py` gives single-CPU rates and CPU time per request.
- `bench/ws_idle/run.py`, memory per idle WebSocket.
- `bench/profile/`, the `LD_PRELOAD` sampling profiler: self time and inclusive time to depth 4, symbolised with `llvm-symbolizer`. Use `--main` for main-thread-only samples.
- `GC_PRINT_STATS=1` for Boehm's statistics.
- `viper build --release --emit llvm-ir` to read the optimised IR.

**Missing**, all planned in [09](09-performance-plan.md) but not built:
- Allocation counters by kind (`VIPER_ALLOC_STATS`).
- Microbenchmarks that isolate one operation each, with Go twins.
- A regression check (`bench/run.py --check` against recorded results).
- Benchmarks with realistic headers, TLS, a database, or open-loop load at a fixed arrival rate.

**Methodology that worked:**
- Build A and B binaries from the same tree by swapping one file.
- Run them interleaved and in alternating order, 3 or more rounds, with the server on CPUs `0-2,6-8` and the load generator on `3-5,9-11`.
- Never run benchmarks while building or testing.

## 8. Environment and constraints

- **Machine.** Laptop, AMD Ryzen AI 5 340 (6 cores, 12 threads), 15 GB, Debian with Linux 6.12. Thermals make it vary by about 10% between runs.
- **Toolchain.** rustc 1.100 (LLVM 23); LLVM 19 for the compiler (`llvm-19-dev`, `LLVM_SYS_191_PREFIX=/usr/lib/llvm-19`); `clang-19`; Go 1.27.4 at `~/.local/go/bin` (use `GOTOOLCHAIN=local`); Python 3.13.5; Boehm `libgc.so.1` 8.2.8 (no headers installed; declarations are written by hand in the runtime).
- **Not available.** `perf` (not installed, and `perf_event_paranoid` is 3), `gdb`, `valgrind`, `strace`.
- **Downloads** (packages, crates, tools) need the owner's explicit approval each time. Cargo runs with `--offline` against the existing registry cache.
- **Repository rules.**
  - The language's name and file extension must not be spelled in Rust source outside `crates/lang_names` (a test enforces it, comments included). Use `CLI_NAME` and `SOURCE_EXTENSION`.
  - Commit and push per milestone, with the `Co-Authored-By` trailer the owner uses.
  - `tests/run/hello` is an untracked file that is not part of the project; leave it alone.
- **Correctness gates for any change:**
  - `cargo test --workspace` (unit tests, about 50 end-to-end programs in `tests/run` run in debug, release and collector-stress modes, the guide's examples, HTTP, TLS and WebSocket tests);
  - `sh tests/stress.sh 300` (concurrency programs, release and collector stress);
  - `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --check`.
- **Semantics to preserve.** Integer arithmetic wraps. Strings are immutable and share bytes. Dicts keep insertion order. Struct values have reference semantics (assignment aliases). A panic in a handler becomes a 500 and the server carries on.

## 9. Questions a plan should answer

1. Is the next goal throughput beyond Go on small requests (system calls, request parsing), parity on `/items`-like handlers (formatting, allocation from runtime code), memory (layout), or tail latency under overload? The benchmarks do not yet measure the last.
2. Should structs gain value semantics in some cases (frozen structs, tuples), so collections can store them inline? This is a language design decision with large memory payoffs.
3. Is a mid-level IR worth building now? It is the stated prerequisite for precise collection, broader escape analysis (closures, lists) and yield-point insertion. Without it, code generation works straight from the HIR.
4. Which benchmarks represent real services better: realistic headers, TLS, a PostgreSQL round-trip, larger JSON, many routes? The plan should add those before optimising for them.
5. What should the regression check guard, and with what noise tolerance, given about 10% run-to-run variance on this machine?

## 10. Where to read more

| Topic | Document |
|---|---|
| The performance plan, its findings and each phase's status | [09-performance-plan.md](09-performance-plan.md) |
| Decisions D82 to D86 with numbers, including what was tried and dropped | [08-decisions-and-risks.md](08-decisions-and-risks.md) |
| Every benchmark run, dated, with what changed | [bench/README.md](../bench/README.md) |
| User-facing performance summary | [web-performance.md](web-performance.md) |
| Runtime design: object model, collector, concurrency | [04-runtime-and-memory.md](04-runtime-and-memory.md) |
| Compiler pipeline and code generation | [03-compiler-architecture.md](03-compiler-architecture.md) |
| Test strategy, optimisation backlog | [07-testing-and-performance.md](07-testing-and-performance.md) |
| Building, flags, environment variables | [building.md](building.md) |

## 11. After the final optimisation pass

The [final optimisation plan](10-final-optimization-plan.md) was carried out on 2026-11-19 (D87 to D90 in [08](08-decisions-and-risks.md); full results in [bench/README.md](../bench/README.md) and [web-performance.md](web-performance.md)).

| Measure | Before the pass | After | Against Go after |
|---|---|---|---|
| `/items`, CPU per request on one core | 9.6 µs | 3.6 µs | 2.84x faster |
| `/json`, CPU per request on one core | 2.7 µs | 1.9 µs | 2.78x faster |
| `POST /orders`, CPU per request on one core | 4.6 µs | 2.9 µs | 2.99x faster |
| `/items`, 256 connections | 229,000 req/s | 528,000 req/s | 2.24x faster |
| `json_orders` | 1.10x faster than Go | 2.97x faster | |
| `word_count` | 1.22x faster than Go | 2.14x faster | |
| `binary_trees` | 1.27x faster than Go | 1.46x faster | |

**Leads from section 6:** 1 (HTTP path without the Rust heap), 2 (float formatting), 3 (system calls per request) and 4 (runtime allocation) are done. 6 (idle WebSocket memory) is explained but not fixed: LTO adds a stack page per parked connection, 14.8 to 18.9 KiB. 7 (dicts) is done for `str` and `int` keys. 5, 8, 9, 10 and 11 were left out, for the reasons in the plan's section 5.

**Where the time goes now** (`bench/profile/`, 256 connections):
- **`/plaintext`:** 74% in the system-call stub, which is the kernel's TCP work for one `recv` and one `send` per request and the polling; 18% in the server's own code, spread over the scheduler, parsing and the response; under 3% each in the collector and in `memcpy`.
- **`/items`:** 30% in system calls; about 35% in `libgc` and the locks it takes (allocation from generated code and unscanned allocations); 23% in the program and runtime, of which JSON escaping 5% and float formatting 3%.
- **On six CPUs the small endpoints are limited by the load generator**, not the server, which keeps about five of its six CPUs busy.

**Guards.** `bench/run.py --check` (each program's time relative to Go's, and its memory) and `bench/http/counts.py --check` (system calls, clock reads and allocations per request) fail on a regression; `GC_STRESS=cache` runs every test program with short free lists.

**What going further would need.** No single user-space cost above about 5% is left that the current architecture can remove. Going further needs one of:
- fewer kernel crossings per request, which only pipelining or batching across connections gives;
- a collector that allocates by bumping a pointer and does not sweep, which means a precise collector (stack maps, then MMTk: weeks of work and a download, see D86);
- objects stored inline in collections (value types), a language design question, and the only thing that closes `sort`'s and `binary_trees`' memory gap with Go.

