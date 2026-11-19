# 09 — Performance plan

An analysis of where Weby's time goes today and a plan to close the remaining gaps to Go. Measured on 2026-11-19, after milestone 12's second part, on the machine in [bench/README.md](../bench/README.md) (Ryzen AI 5 340, 12 threads, LLVM 19.1.7, Boehm 8.2.8, Go 1.27.4).

`perf` is not installed on this machine, so the evidence below comes from reading the optimised IR and machine code (`weby build --release --emit llvm-ir`, `llvm-objdump`), Boehm's own statistics (`GC_PRINT_STATS=1`), and small experiments that change one thing at a time. Times are the best of 5 runs.

## Where things stand

| Benchmark | Weby | Go | Weby vs Go | Gap is mostly |
|---|---|---|---|---|
| fib | 0.019 s | 0.030 s | 1.62x faster | — |
| sort | 0.707 s | 1.112 s | 1.57x faster | — |
| word_count | 0.622 s | 0.722 s | 1.16x faster | — |
| json_orders | 0.514 s | 0.531 s | 1.03x faster | — |
| binary_trees | 0.898 s | 0.769 s | 1.17x the time | Collection, 2x memory per object |
| sieve | 0.067 s | 0.055 s | 1.20x the time | `append` is a runtime call |
| dispatch | 0.045 s | 0.026 s | 1.72x the time | Spilled accumulator around an indirect call |
| spawn | 0.142 s | 0.047 s | 3.05x the time | Locks and heap allocations per task |
| HTTP `/items`, 256 conn. | 219k req/s | 303k req/s | 0.72x | Allocation in the request path |
| HTTP `/plaintext`, 256 conn. | 816k req/s | 545k req/s | 1.50x faster | — |

The weak spots are allocation, the cost of runtime calls on hot paths, dynamic dispatch, and task creation. Pure computation is already at or ahead of Go.

## Findings

### 1. LLVM does not know `lrt_alloc` is an allocator

Struct construction calls `lrt_alloc(size, desc)`, declared with no attributes. To LLVM it is an opaque function that may read and write any memory, so:

- An allocation that never leaves its function is never removed.
- Values stored into a fresh object cannot be forwarded to later loads across other calls.
- The fields are zeroed again after the call (`binary_trees.make` has a `memset` of 16 bytes after each `lrt_alloc`), although `GC_malloc` already returns zeroed memory.

**Experiment.** A loop that builds 50 million two-field structs and only reads their fields back takes **0.345 s**. Re-declaring `lrt_alloc` with these attributes and running the same `default<O2>` pipeline lets LLVM delete the allocation; the loop then folds to a constant, and the program takes **0.001 s**:

- `noalias nonnull align 16` on the returned pointer;
- `allockind("alloc,zeroed")`, `allocsize(0)` and `"alloc-family"`;
- `nounwind willreturn`.

There is one complication. `lrt_alloc` writes the descriptor into the first word, so the memory is not entirely zeroed when it returns. Generated code must therefore store the header itself, after a call that really does return zeroed memory.

### 2. Small objects take up to twice their size

Boehm runs with interior pointers enabled, and therefore adds one byte to every request so that a pointer just past an object does not keep the next one alive. With 16-byte size classes this rounds up hard:

| Requested | Takes (interior pointers on, as now) | Takes (off) |
|---|---|---|
| 16 bytes | 32 | 16 |
| 24 bytes | 32 | 32 |
| 32 bytes | 48 | 32 |

`binary_trees`' node is 24 bytes: an 8-byte descriptor header plus two pointers. It takes 32 bytes, where Go's takes 16. That is why `binary_trees` uses 72 MB against Go's 39 MB, and why the collector has twice as many bytes to mark and sweep.

Two facts limit the fix:

- **Removing the header alone does not help this node:** 16 bytes still takes 32. It does help the half of all sizes that land just past a boundary, for example 40 bytes becoming 32.
- **Interior pointers cannot simply be turned off:** string slices point into the middle of other strings, and LLVM's loop optimisations may keep only a derived pointer live.

The header is never read today: the runtime reads descriptors only from lists and dicts, which keep their own copy.

### 3. Collection is a quarter of `binary_trees` and small elsewhere

| Benchmark | Collections | Time in collections | Share of run time |
|---|---|---|---|
| binary_trees | 60 | 228 ms | about 25% |
| json_orders | 14 | 32 ms | about 6% |
| word_count | 2,442 | 52 ms | about 8% |
| sort | 8 | 14 ms | about 2% |

These figures cover marking only. Boehm sweeps lazily inside allocation, so part of the remaining time is also the collector's. `word_count` collects 2,442 times because its live heap is tiny, but each collection takes about 20 µs, and a larger initial heap made it slower (0.61 s to 0.71 s, from worse cache use). So the collector's tuning is not the problem; the number and size of allocations are.

### 4. Hot operations are runtime calls that LLVM cannot inline

The runtime is a Rust static library built by `rustc`'s LLVM 23. Its bitcode cannot be read by the compiler's LLVM 19, so link-time inlining across the boundary is not available. Every operation below is therefore a full call:

| Operation | Today | Measured or seen |
|---|---|---|
| `list.append` | `lrt_list_push` per element | 20 million appends take 34 ms, against Go's 18 ms, where `append` inlines to a compare and a store |
| `a == b` on strings | `lrt_str_eq` | Called for every route segment and every dict key comparison |
| dict hashing and key equality | Function pointers in the descriptor, called from Rust | An indirect call per hash and per probe; the index uses 8-byte slots |
| f-strings, `str(x)`, `print` | Two collected allocations per f-string: the builder object and its buffer (at least 32 bytes), and three or more calls | `f"item {i}"` in `/items` runs 20 times per request |
| allocation | Generated code → `lrt_alloc` → `GC_malloc` through the PLT → thread-local free list | Two calls before any work is done |

### 5. No alias information, so loops reload what they just loaded

Generated IR carries no type-based alias metadata (TBAA). In the `sieve` inner loop, LLVM cannot tell that storing a `bool` into the list's buffer does not change the list's `len` and `data` fields. It reloads both on every iteration, keeps the bounds check in the loop, and does not vectorise. The `dispatch` loop reloads them as well.

### 6. Dispatch: the accumulator goes through memory twice per call

In `dispatch`, the loop's running total lives in an SSE register. Every register of that kind is clobbered by a call, so it has to be saved to the stack around the indirect call. LLVM's register allocator does this twice per iteration, storing then immediately reloading, which puts two store-to-load forwarding delays (about 5 cycles each) on the loop's critical path.

The methods themselves are three instructions. Go has the same calling-convention constraint but only one round trip.

Rebuilding with `-O3` or `-mcpu=native` does not change this (see finding 9). The real fix is to avoid the indirect call: the program has three implementors of `Shape`, so a compare on the vtable pointer can call, and inline, the right method directly.

### 7. Task creation takes four locks and two Rust heap allocations

`spawn` does all of the following:

- allocates a `Fiber` in an `Arc` and the entry closure in a `Box`, both from Rust's allocator;
- pops a stack from a global, mutex-protected pool;
- links the fiber into a global registry under Boehm's allocation lock;
- pushes to a mutex-protected `VecDeque`;
- allocates the task and its result slot from the collector.

On top of that, **every context switch takes and releases Boehm's global allocation lock twice**, so that a collection never sees a half-finished switch.

For HTTP this is per request, since each keep-alive read parks the connection's fiber. At 800,000 requests per second across 12 workers, that is a global lock acquired about 3 million times a second. Its share has not been measured (that needs `perf`), but it is the likeliest limit on small-response throughput and tail latency as the core count grows.

### 8. The HTTP request path allocates more than it needs to

Before the handler runs, a request:

- splits its path into a new list of new strings (`split_path`);
- tries every route in order, and `match_route` allocates a fresh `dict` for parameters for each route it tries, matching or not;
- string-compares every segment through `lrt_str_eq`.

After the handler, `serve_one` builds the header block by repeated concatenation, which makes one new string per header.

`/items` itself adds, per record:

- an `Item`;
- an f-string, which is two allocations plus its result;
- a two-element list literal, which is the list plus its buffer.

Then come the comprehension's list, the JSON builder's growth, and the body. That is about 130 allocations per request in all.

### 9. What does not help

| Change | Effect |
|---|---|
| `default<O3>` instead of `default<O2>` | None beyond noise on any benchmark |
| `-mcpu=native` instead of `generic` | None beyond noise (`sieve` 0.068 s to 0.070 s, `binary_trees` 0.888 s to 0.906 s) |
| A larger initial heap | Slightly better for `json_orders` and `sort` (about 5%), worse for `word_count` and `binary_trees` |
| Compile time | Not a problem: 0.12 s to 0.27 s per program, release or debug, mostly linking |

So the gains are not in LLVM's flags. They are in what the compiler tells LLVM, and in how much the program allocates and calls.

## The plan

Ordered by payoff per unit of effort. Each phase ends by re-running `bench/run.py` and `bench/http/run.py` and recording the results in `bench/README.md`, as the earlier milestones did.

### Phase 1 — Tell LLVM what it needs to know (days) — done 2026-11-19

Done as planned except item 2, which covers only the string functions that cannot panic so far; see D82 in [08](08-decisions-and-risks.md) and the results in [bench/README.md](../bench/README.md). `sieve` and `sort` gained the most (`sieve` is now faster than Go). `/items` moved only from 0.72x to 0.74x Go, so phases 2 and 3 remain the route there. The work also found and fixed a reference to collected memory held only from the Rust heap (the JSON reader), and added release builds under collector stress to the test suite.

These are small code-generation changes with no change to layouts or the runtime's design.

1. **Allocator attributes.** Add a runtime function `lrt_alloc_zeroed(size)` returning zeroed, scanned memory; generated code then stores the descriptor itself. Declare it with:
   - `noalias nonnull align 16` on the result;
   - `allockind("alloc,zeroed")`, `allocsize(0)`, `"alloc-family"="weby_gc"`;
   - `nounwind willreturn`.

   Do the same for `lrt_alloc_raw` (closure environments and captured variables) and the atomic allocation used by strings.

   *Expected:* temporary structs, boxed captures and closure environments that do not escape their function disappear entirely, as in the experiment. The redundant `memset` after each allocation goes too.
2. **Attributes on the rest of the runtime.** Mark functions that cannot panic or unwind `nounwind`. Mark pure ones `memory(read)` or `memory(argmem: read)` (`lrt_str_eq`, `lrt_str_hash`). Add `noundef` and `nonnull` where they hold. This lets LLVM move these calls out of loops and merge repeated ones.
3. **TBAA metadata** on loads and stores of list headers (`len`, `cap`, `data`), dict headers, struct fields by field type, and element stores. *Expected:* `sieve`'s inner loop keeps `len` and `data` in registers and can vectorise; the same applies to every loop over a list.
4. **Inline the `append` fast path** in generated code: if `len < cap`, store the element and increment `len`; otherwise call `lrt_list_push`. Do the same for indexing in `for x in list` and for `len()`. *Expected:* `sieve` at or below Go's time.
5. **Inline string equality:** compare lengths, then the pointers, then `memcmp`, which LLVM turns into `bcmp` and inlines for small constant lengths. This helps routing and dict keys.
6. **Builders on the stack.** The `Builder` record (pointer, length, capacity) becomes an `alloca` in the function that builds the f-string, not a collected object. For f-strings, compute a size estimate from the literal parts plus a fixed width per formatted value, so the buffer is allocated once at about the right size. *Expected:* `f"item {i}"` goes from three allocations to one.

Exit: every test passes, including under the collector's stress mode. Allocation counts drop on `/items` and `json_orders`; `sieve` is at or below Go's time.

### Phase 2 — Allocate less (one to two weeks)

1. **Escape analysis on the typed IR.** This is intraprocedural to start with. A struct, list literal, closure environment or builder whose reference is never:
   - stored into the heap,
   - returned,
   - captured by an escaping closure,
   - passed to a call that is not inlined and whose parameter is not known not to escape,

   is created with `alloca` instead of `lrt_alloc`. Allocator attributes (phase 1) already handle the cases LLVM can see after inlining. This handles the ones it cannot, such as an object passed to a small helper that is not inlined.

   Per-function parameter summaries, computed bottom-up over the call graph, extend this across calls. Most of the payoff is in handlers: request-scoped records, intermediate lists, and the parameter dicts in routing.
2. **Pointer-free structs in pointer-free memory.** A struct with no reference fields (only numbers and bools) is allocated with `GC_malloc_atomic`, so the collector never scans it. Strings already work this way.
3. **The object header (decision needed).** It costs 8 bytes per struct and enum object and is never read. Options:
   - **(a)** Drop it from structs and keep it in lists and dicts. When a precise collector arrives, it uses its own side metadata.
   - **(b)** Keep it, as [04](04-runtime-and-memory.md) decided.

   (a) saves memory on the half of all sizes that sit just past a 16-byte boundary, but not on 24-byte objects like `binary_trees`' node (finding 2). Recommendation: decide together with phase 5, since a precise collector is what the header was kept for.
4. **An inline allocation fast path.** Keep a per-worker free list for each small size class, refilled in batches with `GC_malloc_many`. Generated code pops from it inline: a thread-local load, a compare, and a store. It calls the runtime only when the list is empty. This is Boehm's documented `gc_inline.h` technique, and it removes both calls from every small allocation.

Exit: `/items` at 256 connections at or above Go's rate; `binary_trees` within 1.1x of Go.

### Phase 3 — Runtime: scheduler, tasks, HTTP (one to two weeks)

1. **Take the collector's lock off the context switch.** Each worker publishes "which fiber is running and where its stack ends" through per-worker fields. The collector's "push other roots" hook reads them once the world is stopped: Boehm stops every thread with a signal, so the fields are stable by then.

   This needs a careful proof against the case where a thread is stopped mid-switch. The switch can be made to update the fields in a fixed order, and the hook then scans both the old and the new stack ranges if it sees a switch in progress, which is conservative and therefore safe.

   *Expected:* higher small-response throughput at high core counts, and a flatter p99. Measure before and after; this is also the change to try first once `perf` is available.
2. **Cheaper `spawn`:**
   - put the fiber record at the top of its own stack (no `Arc` allocation);
   - give each worker its own stack cache, falling back to the global pool;
   - keep a per-worker fiber registry, linked without the collector's lock: it is read only when the world is stopped;
   - store the entry as a function pointer plus environment instead of a boxed closure;
   - replace the mutex-protected run queues with a work-stealing deque (Chase–Lev).

   *Expected:* `spawn` within 2x of Go.
3. **A compiled router.**
   - Build a segment tree when `serve` starts.
   - Match the request path by walking it in place, without splitting it into a list.
   - Allocate the parameters dict only for routes that have parameters, and only once the route has matched.
   - Move the router into the runtime or keep it in Weby; either way it must not allocate per route tried.
4. **Response headers** written straight into the response buffer by `respond_raw` from the response's header dict, instead of concatenating a string per header in `serve_one`.

Exit: `spawn` within 2x of Go. HTTP p99 at 256 connections no worse than now, with higher throughput.

### Phase 4 — Code generation for dispatch and collections (two to three weeks)

1. **Devirtualisation.** When a trait has at most four implementors in the whole program (the compiler sees the whole program), a call on a trait object compares the vtable pointer against each implementor's vtable and calls the method directly, keeping the indirect call as the fallback. LLVM can then inline the method, and the accumulator stays in a register. Where the concrete type is known at the call site, the call is direct with no compare. *Expected:* `dispatch` at or below Go's time.
2. **Runtime fast paths in LLVM 19 bitcode.** Hot helpers that are awkward to emit by hand would be written in C:
   - string hashing;
   - the probe loop of the dict;
   - UTF-8 checks.

   They would be compiled with the installed `clang-19` to bitcode when the compiler is built, embedded in the compiler, and linked into each program's module before optimisation, so they inline into generated code. This works around the `rustc` (LLVM 23) and compiler (LLVM 19) version mismatch without changing either.
3. **Monomorphised dicts** (backlog item 9). For each key type, generate the probe loop with the hash and equality inlined, using 4-byte index slots that hold a few bits of the hash so a probe rarely touches the entry. The table layout stays as it is, so the runtime keeps handling growth and iteration.
4. **Concatenation chains** `a + b + c + ...` sized once and written once (backlog item 8).

Exit: `dispatch` and `word_count` at or ahead of Go.

### Phase 5 — The collector (decided after phase 2)

After phases 1 and 2, measure again how much time and memory the collector costs on `binary_trees`, `/items` and a long-running server.

- If collection is still more than about 15% of `/items`, or memory is still about 2x Go's, take up the precise, generational collector ([04](04-runtime-and-memory.md#beyond-boehm); MMTk is the candidate). That fixes the size-class rounding in finding 2, allows bump allocation, and makes the object header useful.
- If not, stay on Boehm.

This is the largest and riskiest item, which is why it comes last and is decided by measurement.

## Measurement work to do alongside

- **Install `perf`** (the `linux-perf` package). This is a download, so it needs your approval. With frame pointers and line tables already in generated code (D67), it gives function-level profiles and flame graphs. It is needed to measure finding 7 and to confirm the rest.
- **Allocation counters:** a `WEBY_ALLOC_STATS=1` mode that prints, at exit, the number of allocations and bytes by kind (struct, list, string, builder, closure). Phases 1 and 2 are judged by these numbers, not only by time.
- **Microbenchmarks** in `bench/` for the operations above, each with a Go twin:
  - `append` into a fresh list;
  - a temporary struct in a loop;
  - an f-string in a loop;
  - a string-keyed dict lookup;
  - a trait call;
  - `spawn` and join.

  The full benchmarks mix several costs; these isolate one each.
- **A regression check:** `bench/run.py --check` compares against the last recorded results and fails on a slowdown over 10%, so later work cannot undo these gains unnoticed.

## Expected outcome

These are estimates to steer by, not promises.

| Benchmark | Now vs Go | After phases 1–4 |
|---|---|---|
| sieve | 1.20x the time | at or below Go |
| dispatch | 1.72x the time | at or below Go (devirtualised) |
| binary_trees | 1.17x the time | about 1.1x; parity needs phase 5 |
| spawn | 3.05x the time | within 2x |
| HTTP `/items` | 0.72x | at or above 1.0x |
| HTTP `/plaintext`, `/json` | 1.5x faster | 1.5x faster or better |
| word_count, json_orders, sort, fib | ahead | ahead by more |
