# Final optimisation plan

The plan for one last round of performance work, written for the agent that will implement it. It follows the five phases of the [performance plan](09-performance-plan.md) and starts from the state described in the [optimisation handoff](optimization-handoff.md). Read the handoff first: this document does not repeat the architecture, the environment or the lessons recorded there. Planned on 2026-11-19 at commit `154ff07`; nothing here is implemented.

## In short

- **Five milestones.** Four of work and one to close out. Each ends in a commit, a decision record and measured numbers.
- **Where the time is.** Fresh profiles and call counts (section 2) show four places where user-space time is still wasted:
  1. the runtime uses the Rust heap and `String`s on hot paths (float formatting, JSON, HTTP parsing): 155 `malloc` calls per `/items` request;
  2. every HTTP request makes two `recv` calls that fail and five clock reads, about 10% of a small request;
  3. runtime code allocates collected memory through out-of-line calls and grows buffers by doubling: 75 calls and about 8 KB per `/items` request where about 5 KB would do;
  4. string-keyed dicts spend 45% of `word_count` in hashing and probing that can be made about twice as cheap.
  5. the runtime is built without link-time optimisation. Turning it on was measured during planning: `json_orders` 6.5% faster, HTTP endpoints 2% to 4%, executables a third smaller, with no code change.
- **Expected result.** `/items` from about 1.05x Go to about 1.4x; small endpoints from about 2x to about 2.3x; `json_orders` about 25% faster; `word_count` about 15% faster; `binary_trees` about 20% less wall time.
- **Why this can be the last pass.** After it, 65% to 75% of a small request is kernel time for one `recv` and one `send`, and no user-space item above about 3% is left that does not need a change of architecture (section 5).

| Milestone | Theme | Main files | Size |
|---|---|---|---|
| [1](#milestone-1-measuring-tools-then-formatting-and-json-without-the-rust-heap) | Measuring tools; LTO for the runtime; float formatting and JSON without the Rust heap | `Cargo.toml`, `lang_format`, `lang_rt/src/{json,builder,strings}.rs`, `lang_codegen/src/json.rs`, `bench/` | Medium |
| [2](#milestone-2-the-http-connection-path) | HTTP connection path: system calls, clock, parsing, response | `lang_rt/src/{http,poll,ws}.rs` | Large |
| [3](#milestone-3-allocation-and-the-collector) | Allocation from runtime code, presizing, parallel marking | `lang_rt/src/{gc,list,tasks}.rs`, `lang_codegen/src/{alloc,expr,json}.rs`, `lang_sema` | Large |
| [4](#milestone-4-generated-code-builders-and-dicts) | Generated code: inline builder pushes, dict entry points and hashing | `lang_codegen/src/{expr,json,builtins}.rs`, `lang_rt/src/{dict,strings}.rs` | Medium |
| [5](#milestone-5-close-out) | Full measurements, documents, regression check | `docs/`, `bench/` | Small |

Do them in this order. Milestone 1 builds the tools the others are judged with. Milestones 1 and 4 both touch JSON encoding, and 4 assumes 1 is done.

## 1. Goal and scope

**Goal:** remove the remaining avoidable user-space cost in the runtime and in generated code, without changing the language, the collector or the object layout.

**In scope:** the runtime library, the code generator, the lowering of comprehensions in `lang_sema`, the benchmark tooling.

**Out of scope** (section 5 gives the reasons): value types in collections, small-string optimisation, a precise or moving collector, `io_uring`, HTTP/2, a mid-level IR.

**Targets.** CPU time per request on one logical CPU (`bench/http/one_core.py`), and best-of-3 times for the batch programs. "Now" is from [web-performance.md](web-performance.md).

| Measure | Now | Target | Mostly from |
|---|---|---|---|
| `/plaintext` | 2.6 µs | 2.25 µs or less | Milestone 2 |
| `/json` | 2.7 µs | 2.3 µs or less | 2 |
| `/items` | 9.6 µs | 7.2 µs or less | 1, 3, 4 |
| `/users/42` | 3.2 µs | 2.75 µs or less | 2 |
| `POST /orders` | 4.6 µs | 3.8 µs or less | 1, 2 |
| `json_orders` | 0.52 s | 0.40 s or less | 1 |
| `word_count` | 0.65 s | 0.56 s or less | 4 |
| `binary_trees` | 0.65 s | 0.55 s or less (wall time) | 3 |
| Peak memory, every benchmark | see handoff | no more than 5% worse | all |

The machine varies by about 10% between runs, so a target counts as met when an interleaved A/B run (section 4) shows the ratio, not when one run shows the absolute figure.

## 2. Fresh evidence

All measured on 2026-11-19 with the release build of `154ff07`. Servers were pinned to CPUs `0-2,6-8` and the load generator to `3-5,9-11`, with 256 connections.

### 2.1 Profiles

`bench/profile/` sampler; percentages of all samples in the process.

| Workload | System-call stub | Program and runtime | `libgc` | What stands out |
|---|---|---|---|---|
| `/items` | 23.1% | 31.4% | 20.6% | Float formatting 11.8% to 13.3% inclusive (`float_repr`, `core::fmt`, Grisu); `malloc` + `free` 6.3%; `push_json_string` 2.2%; `lrt_builder_push` 2.1% |
| `/plaintext` | 65.2% (`write_all` 41.9%, `read` 14.6%) | 22.6% | 2.7% | `IoSource::wait` 4.0%; `clock_gettime` 3.1%; `malloc` + `free` 2.9%; `response_bytes` 2.1% |
| `/users/42` | 57.7% | 21.2% | 9.6% | `serve_one` 6.1% inclusive; `GC_malloc_kind` 2.9%; `fits` 1.4% |
| `POST /orders` | 42.9% | 33.0% | 8.2% | `free` 3.6%; `malloc` 2.9%; `scan_string` 2.7%; `Vec` growth 2.6%; `from_utf8` 1.9% |
| `json_orders` | none | 62.5% | 19.0% | Float formatting 17.6% inclusive; Rust heap about 17% (`free` 6.8%, `malloc` 3.6%, `Vec` growth 5.6%); `from_utf8` 4.2%; `scan_string` 4.2% |
| `word_count` | none | 75.2% | 15.4% | `Dict::hash` 30.7% self; `Dict::probe` 13.3%; `lrt_str_split_ws` 11.9%; `change_case` 5.9%; `lrt_dict_slot` 4.8% |
| `binary_trees` | none | 39.8% | 60.1% | `libgc` is almost all the mark loop, on one thread |
| `sort` | none | | | About 47% in `memcmp` under `lrt_str_cmp`: cache misses on the string bytes. Already 2.2x Go. No cheap fix |
| `sieve` | none | 81.5% in `main` | | Index code keeps a select and a bounds check per access |

### 2.2 Calls per request

Counted with an `LD_PRELOAD` shim (appendix A) and divided by the requests the load generator reported. These are exact and repeatable, unlike timings.

| Endpoint | `recv` (failing) | `send` | `clock_gettime` | `epoll_wait` | Rust `malloc` | `GC_malloc` | `GC_malloc_atomic` | Free-list refills |
|---|---|---|---|---|---|---|---|---|
| `/plaintext` | 3.0 (2.0) | 1.0 | 5.0 | 0.15 | 9.4 | 1 | 2 | 0.10 |
| `/json` | 3.0 (2.0) | 1.0 | 5.0 | 0.12 | 10.3 | 1 | 3 | 0.11 |
| `/items` | 3.0 (2.0) | 1.0 | 5.0 | 0.02 | 155.1 | 46 | 29 | 1.62 |
| `/users/42` | 3.0 (2.0) | 1.0 | 5.0 | 0.04 | 12.1 | 7 | 8 | 0.42 |
| `POST /orders` | 3.0 (2.0) | 1.0 | 5.0 | 0.02 | 62.1 | 4 | 28 | 0.49 |

Allocations that generated code makes inline do not call the collector and are not in the `GC_malloc` columns; each refill serves many of them.

**What the counts say:**
- **Two of three `recv` calls fail.** Each returns `EAGAIN`. The cause is in section 6.1.
- **Five clock reads per request.** One at the start of `read_request`, two in `IoSource::wait`, one in `write_all`, one `SystemTime::now()` in `http_date`.
- **The `/items` `malloc` count is float formatting.** 20 floats at about six `String`s each is about 120 of the 155; 20 more are the `Vec` in `lrt_builder_push_json_str`; about 10 are the HTTP path.
- **The `/items` collector calls can be accounted for exactly:**
  - `GC_malloc` 46 = 20 tag lists at 2 each (header and buffer) + the outer list's header + its 4 growth buffers (the comprehension starts at capacity 0) + the `Response` headers dict.
  - `GC_malloc_atomic` 29 = 20 f-string buffers + 7 growth steps of the JSON output builder (32 bytes doubling to 2,048 for a 1.1 KB body) + the method and the path.
- **The JSON output builder is about 45% of the collected bytes of an `/items` request.** Its growth steps allocate 4,064 bytes in total; the rest of the request allocates about 4.5 KB. Presizing it removes six allocations, six copies and a third of the allocation volume.

### 2.3 Unit costs on this machine

| Operation | Cost |
|---|---|
| `recv` on a socket with nothing to read | 80 ns |
| `clock_gettime(CLOCK_MONOTONIC)` | 19.3 ns |
| `clock_gettime(CLOCK_MONOTONIC_COARSE)` | 2.3 ns |

Two failing `recv` calls and five clock reads cost about 257 ns of a 2,600 ns small request: 10%.

### 2.4 Parallel marking for batch programs

Boehm's parallel marker is compiled into `libgc.so.1`, but marker threads only start at `GC_allow_register_threads()`, which the runtime calls at the first `spawn` (`tasks.rs`, `allow_threads`). A batch program that never spawns marks on one thread. Enabling threads at start with a preload shim (appendix A) gave:

| Benchmark | Wall time before | After | User CPU |
|---|---|---|---|
| `binary_trees` | 0.65 s | 0.51 s (-22%) | 0.65 s to 0.94 s |
| `json_orders` | 0.50 s | 0.48 s (-4%) | |
| `sort` | no change | | |
| `word_count` | 0.61 s | 0.63 s (+4%) | |

`word_count` loses because its heap is 13 MB: it pays the allocation lock and has nothing to mark. A heap-size threshold separates the cases.

### 2.5 Generated code

From `weby build --release --emit llvm-ir` on `bench/http/server.wb` and `bench/sieve.wb`:
- JSON encoding emits one out-of-line `lrt_builder_push` call per constant piece. Keys are merged with their punctuation (`{"id":`), but neighbours such as `]` then `}` are separate calls.
- The JSON output builder starts empty.
- A comprehension over `range(20)` calls `lrt_list_new(desc, 0)`. A list literal calls `lrt_list_new(desc, n)` and then does a capacity check for each element.
- `lrt_alloc_raw` call sites are inlined as designed; `lrt_alloc_atomic_raw` (pointer-free structs) is still an out-of-line call.
- `list[i]` is a compare, a select (negative index) and an unsigned compare (`expr.rs:526`).

### 2.6 The runtime's build profile

The workspace `Cargo.toml` has no `[profile.release]` section, so the runtime is built with 16 code-generation units and no link-time optimisation. `lang_format` is a separate crate, so `float_repr` and its helpers cannot inline into `lrt_builder_push_float`. Building the runtime with fat LTO and one unit made `json_orders` 6.5% faster and the HTTP endpoints 2% to 4% faster, and executables 32% smaller (milestone 1, item 1.2).

## 3. Where the time can be saved

| Finding | Evidence | Milestone | Expected gain |
|---|---|---|---|
| The runtime is built without LTO, in 16 units | Section 2.6 | 1 | Measured: `json_orders` 6.5%, HTTP 2% to 4% |
| Float formatting builds about six `String`s per float | 12% to 18% inclusive on `/items` and `json_orders` | 1 | `/items` 6% to 8%; `json_orders` 12% to 15% |
| JSON encode and decode go through `Vec` and `String` | `malloc`/`free`/growth 9% on `POST /orders`, 17% on `json_orders` | 1 | `POST /orders` 8%; `json_orders` 10% to 15% |
| Two failing `recv` per request | Section 2.2 | 2 | 6% on small endpoints |
| Five clock reads per request | Sections 2.2, 2.3 | 2 | 3% to 4% on small endpoints |
| Request parsing copies everything into `String`s | 9 to 12 `malloc` per request, two more per header | 2 | 3% to 5% bare; much more with browser headers |
| Response built in a fresh `Vec`, date cloned under a mutex | `response_bytes` 2.1% | 2 | 2% |
| Runtime code calls `GC_malloc` out of line | 75 calls per `/items` request | 3 | `/items` 4% to 6%; `POST /orders` 3% to 5% |
| Buffers start small and double | 4,064 bytes for a 1.1 KB body | 3 | `/items` 4% to 6% |
| Batch programs mark on one thread | Section 2.4 | 3 | `binary_trees` 20% wall time |
| One call per constant JSON piece | Section 2.5 | 4 | `/items` 3% to 5% |
| Dict hashing and probing for `str` keys | 49% of `word_count` | 4 | `word_count` 10% to 20% |

## 4. Rules for the work

**Constraints** (from the handoff, section 8; they still hold):
- No downloads without the owner's approval. Cargo runs with `--offline`. Adding a crate to `Cargo.toml` needs approval even when the crate is already in the local registry cache.
- The language's name and file extension are never spelled in Rust source outside `crates/lang_names`, comments included. Use `CLI_NAME`, `SOURCE_EXTENSION` and `lang_names::env_var(...)`. Environment variables in this document are written with the current prefix for readability only.
- `tests/run/hello` is an untracked file that is not part of the project. Do not commit it.
- Commit and push after each milestone.

**Correctness gates, for every commit:**

```bash
cargo test --workspace --offline
```

```bash
sh tests/stress.sh 300
```

```bash
cargo clippy --workspace --all-targets --offline -- -D warnings
```

```bash
cargo fmt --check
```

**Semantics to preserve:** everything in the handoff's section 8, and in particular the exact text of `repr(float)`, of f-string formatting and of JSON error messages and paths. Tests compare these byte for byte.

**How to measure:**
1. **Counts first.** Every item below names a count that must move: `recv` per request, `malloc` per request, collector calls per request, bytes allocated per request. Counts are exact on this machine; timings are not. An item whose count moved as designed is done even if the timing gain is inside the noise.
2. **Timings by interleaved A/B.** Keep the baseline runtime: copy `target/release/liblang_rt.a` aside at the start of each milestone, and build the baseline server with `WEBY_RUNTIME=<that copy>`. For compiler changes, keep the baseline compiler binary too. Run A and B alternately (A B B A A B), three rounds or more of 5 s, pinned as in section 2. Compare medians. Milestone 1 automates this.
3. **Never benchmark while building or testing.**
4. **Check peak memory** (`bench/run.py` and `bench/http/run.py` report it) whenever an item changes allocation.
5. **Drop what does not pay.** An item that shows neither a count change nor 2% in A/B is reverted, and the decision record says so. Several items below are marked optional for this reason.

**Decision records:** one per milestone in [08-decisions-and-risks.md](08-decisions-and-risks.md), D87 to D90, in the style of D82 to D86: what was done, the numbers, what was tried and dropped.

## Milestone 1: Measuring tools, then formatting and JSON without the Rust heap

### 1.1 Tools

Build these first; later milestones use them as exit criteria.

- **`bench/profile/count.c`**, the call-counting shim in appendix A, and **`bench/http/counts.py`**, which runs each endpoint under it and prints the table of section 2.2. Document both in `bench/README.md`.
- **Bytes allocated.** With `WEBY_ALLOC_STATS=1`, print `GC_get_total_bytes()` and `GC_get_gc_no()` to standard error at exit (`gc.rs`; both functions are exported by `libgc`). No cost on any hot path. `counts.py` divides by the request count.
- **`bench/ab.py`.** Takes two executables (two servers, or two batch programs), runs them interleaved in alternating order with the pinning of section 2, and prints each side's median, the ratio and the spread. For servers it takes the endpoint list from `bench/http/run.py`.
- **A browser-like load mode.** `bench/http/load.go` gets a flag that adds about twelve typical request headers, about 600 bytes (`User-Agent`, `Accept`, `Accept-Language`, `Accept-Encoding`, `Cookie`, `Referer`, `Cache-Control`, the `Sec-Fetch-*` set). `bench/http/run.py --browser` passes it. The current load sends only `Host`, which understates parsing cost. Record the baseline in this mode before milestone 2.

### 1.2 The runtime's build profile

Build the runtime with `lto = "fat"` and `codegen-units = 1`. The planning measurement used this command, which changes nothing in the repository:

```bash
cargo build -p lang_rt --release --offline --target-dir /tmp/rt-lto --config 'profile.release.lto="fat"' --config 'profile.release.codegen-units=1'
```

**Measured during planning** (2026-11-19). Interleaved A/B over six runs, with programs built from the same source against each runtime. Batch programs and the server were on one CPU, the server with 64 connections:

| Benchmark | Current runtime | Fat LTO, one unit | Change |
|---|---|---|---|
| `json_orders` | 0.611 s | 0.571 s | 6.5% faster |
| `word_count` | 0.695 s | 0.694 s | none |
| `/items` | 95,180 req/s | 98,580 req/s | 3.6% more |
| `/plaintext` | 354,700 req/s | 362,600 req/s | 2.3% more |
| `POST /orders` | 198,200 req/s | 203,100 req/s | 2.5% more |
| Server executable | 7.6 MB | 5.2 MB | 32% smaller |

On every row except `word_count`, the two sides' ranges did not overlap. Building the runtime alone took 20 s.

**Adopt it as the first change of milestone 1**, so that every later A/B starts from it:
- Cargo applies `lto` to a whole profile, not to one package, so `[profile.release] lto = "fat"` would also link the compiler with LTO. Measure what that does to `make release`.
- If the compiler's build becomes too slow, build the runtime with its own profile instead (`[profile.runtime]`, inheriting `release`), and have the `Makefile` copy `liblang_rt.a` next to the compiler, where `find_runtime` (`lang_cli/src/main.rs`) looks for it.
- `codegen-units = 1` can be set for `lang_rt` and `lang_format` alone with `[profile.release.package.<name>]`.
- Record the build-time cost in D87.

### 1.3 Float formatting without allocation

**Now** (`lang_format/src/lib.rs`): `float_repr(x) -> String` calls `shortest_digits`, which does `format!("{x:e}")`, `split_once('e')`, `mantissa.replace('.', "")` and `exp.parse()`. Then `fixed_from_digits` or the scientific branch builds the result with more `format!` and `"0".repeat(..)`. The callers (`lrt_builder_push_float` in `builder.rs:145`, `lrt_builder_push_json_float` in `json.rs:792`) copy the `String` into the builder.

**Change, stage 1:**
- Add `pub fn write_float_repr(x: f64, out: &mut [u8; 32]) -> usize` to `lang_format`. The longest result is 24 bytes (`-1.2345678901234567e-308`).
- Get the digits without the heap: a small cursor over a stack array that implements `core::fmt::Write`, then `write!(cursor, "{:e}", x.abs())`. Read the digits and the exponent from that array in place, with a hand-written exponent parse.
- Lay the result out in `out` by copying digit runs and writing zeros, following the same rules as today: fixed notation for a decimal exponent from -4 up to 15, scientific outside, `.0` for whole numbers, `inf`, `-inf`, `nan`, `-0.0`. Read the current code for the exact rules and keep them.
- `float_repr` becomes a wrapper that makes a `String` from the buffer, for callers that want one (the compiler uses `lang_format` too).
- The two runtime callers write the buffer straight into the builder.
- Do the same for the JSON variant: check what `lrt_builder_push_json_float` does today for non-finite values and keep it.

**Stage 2, a fast path for short decimals.** Most floats in real data are short decimals (prices, measurements). For `1e-4 <= |x| < 1e15`, try `k` from 0 to 9: let `m = (|x| * 10^k).round()`; if `m < 1e15` and `m as f64 / 10^k as f64 == |x|`, the digits are `m` with `k` decimals.
- **Why it is exact:** `m` and `10^k` are exactly representable, and IEEE division is correctly rounded, so the test passing means the decimal reads back as `x`. The smallest such `k` gives the shortest digits, and with `m < 1e15 < 2^50` the product's rounding error is under 0.25, so the right `m` is never missed at a smaller `k`.
- **When it does not apply,** fall through to stage 1.
- **Do stage 2 only if** `core::fmt`'s float code is still above 2% of `/items` after stage 1.

**Not chosen:** a shortest-digits crate. `zmij` 1.0.23 happens to be in the local cargo cache, but it is not a dependency of the project, and adding one needs the owner's approval. Ask only if stage 2 leaves `core::fmt` above 2%.

**Tests:**
- The existing `repr_matches_python` and `floats_match_python` must pass unchanged.
- Keep the old implementation under `#[cfg(test)]` and compare it with the new one on a million random bit patterns, a million random short decimals (`n / 10^k`), and the edge cases: zeros, subnormals, `f64::MAX`, `f64::MIN_POSITIVE`, powers of ten from `1e-320` to `1e308`, values just either side of `1e16` and `1e-4`.

**Count that must move:** Rust `malloc` per `/items` request from 155 to about 35.

### 1.4 JSON encoding

**Now** (`json.rs:751`): `lrt_builder_push_json_str` allocates `Vec::with_capacity(len + 2)`, calls `push_json_string(&mut out, text)`, then copies the `Vec` into the builder.

**Change:**
- Reserve `len + 2` in the builder, write the opening quote, then copy runs of bytes that need no escape directly. Find the next byte that needs an escape (`"`, `\`, below 0x20) eight bytes at a time; most strings have none, and become one `memcpy`.
- Keep `push_json_string` for its other callers by making the core generic over a small sink trait implemented for `Vec<u8>` and for `Builder`.
- What is escaped must not change. The JSON tests and the JSONTestSuite run are the check.

**Count:** `malloc` per `/items` request to about 15.

### 1.5 JSON decoding

**Now:**
- `lrt_json_reader_new` (`json.rs:354`) boxes a `Reader` holding two `Vec`s and an `Option<(String, String)>`; `lrt_json_reader_free` drops it. Generated code creates and frees it at `lang_codegen/src/json.rs:729` and `:754`.
- `scan_string` (`json.rs:175`) builds a `Vec<u8>`, turns it into a `String` (`from_utf8`), and then `lrt_json_read_str` and `lrt_json_object_key` copy it again into collected memory with `Str::from_bytes`.
- `lrt_json_push_key` (`json.rs:412`) does `to_string()` on every field name, to track the path for error messages.
- `scan_number` parses integers with `text.parse::<i64>()` after scanning them.

**Change:**

1. **The reader on the stack.** Make `Reader` a `repr(C)` record of fixed size that generated code allocates in its frame, as it does for `Builder`. Add `lrt_json_reader_init(r, ptr, len)`; keep `lrt_json_reader_free` to drop the rare heap parts.
   - `frames` becomes a bit set: `MAX_DEPTH` is 512, so eight words hold one bit per open container.
   - `path` becomes an inline array of eight parts, each `{ptr, len}` for a key or `{null, index}` for an index, with an `Option<Box<Vec<..>>>` for deeper documents so that error paths stay complete.
   - `error` becomes an `Option<Box<(String, String)>>`, allocated only on failure.
   - Share the size between the crates as a constant, with a `const` assertion in the runtime that `size_of::<Reader>()` matches it.
   - The reader then sits on a stack the collector scans, so the input text and the borrowed keys stay alive through it. Keep the input argument of `lrt_json_reader_free` anyway.
2. **Strings copied once.** The input is a `str`, so it is already valid UTF-8; only `\u` escapes can produce something invalid.
   - No escapes (the common case, found by the same eight-bytes-at-a-time scan as 1.4): allocate `len` bytes with `gc::alloc_atomic` and `memcpy`. No `from_utf8`.
   - With escapes: allocate the escaped length (the result is never longer), decode straight into it, and return the shorter length.
3. **Keys of struct fields without a copy.** Add `lrt_json_object_key_view`, which returns a view into the input when the key has no escapes, and use it in `decode_fields` (`lang_codegen/src/json.rs:542`), where the key is only compared. A key with escapes falls back to a decoded copy.
   - Keep the copying `lrt_json_object_key` for `dict[str, V]` decoding (`json.rs:410` in the code generator), where the key is stored: a stored view would keep the whole input alive.
   - String *values* are always copied, for the same reason.
4. **Path tracking without allocation.** `lrt_json_push_key` stores the pointer and length. Field names are constants in the program, and dict keys are collected strings that the dict being built keeps alive.
5. **Integers.** Accumulate the value while scanning the digits, with an overflow check, instead of scanning and then parsing. Leave floats on `parse::<f64>()`.
6. **Optional: field matching from the expected position.** `decode_fields` compares each key with the fields in order from the first. For input in declaration order that is n²/2 comparisons for n fields. Only if the comparison chain shows in a profile of `POST /orders` after the items above: keep the index of the field after the last match and try it first.

**Tests:** the JSON unit tests and `tests/run` programs; JSONTestSuite if it is present locally (D81); new cases for error paths nested deeper than eight levels, keys with escapes matched against struct fields, strings that end in an incomplete escape, lone surrogates, and integers at and just beyond the `i64` limits.

**Count:** `malloc` per `POST /orders` request from 62 to about 12 (what is left is the HTTP path, removed in milestone 2).

### 1.6 Other `String` round trips in the runtime

Cheap ones, in the same style:
- `lrt_str_join` (`strings.rs:322`) allocates a `String` per piece although it has already allocated the exact output. Copy the pieces directly.
- Implement `core::fmt::Write` for `Builder`, and let `lrt_builder_push_str_repr` and `lrt_builder_push_format_{int,float,str}` write through it instead of building a `String` first. In `lang_format`, give `format_int`, `format_float` and `format_str` variants that write to a `fmt::Write`. Lower priority: do the ones that are a mechanical change.

### Exit criteria

| Measure | Before | After |
|---|---|---|
| Runtime built with LTO | No | Yes, build time recorded |
| Rust `malloc` per `/items` request | 155 | 15 or fewer |
| Rust `malloc` per `POST /orders` request | 62 | 15 or fewer |
| `json_orders` | 0.52 s | 20% faster or better in A/B |
| `/items` | | 10% faster or better in A/B |
| `POST /orders` | | 5% faster or better in A/B |

Record as D87.

## Milestone 2: The HTTP connection path

### 2.1 One `recv` per request

**Now.** `Direction::ready()` (`poll.rs:142`) sets the `ready` flag *and* wakes the waiter. The woken task returns from `IoSource::wait` (`poll.rs:218`) with the flag still set. The sequence for each request is then:
1. `recv` succeeds (the request).
2. After the response, `recv` fails with `EAGAIN`; `wait` sees the stale flag, clears it and returns at once.
3. `recv` fails again; `wait` parks.

**Change 1: the flag means "an edge nobody has consumed".**
- `ready()`: under the `waiter` mutex, if there is a waiter, take it and wake it and leave the flag alone; otherwise set the flag.
- `wait()`: under the same mutex, if the flag is set, clear it and return `Retry`; otherwise store the waker. Release the mutex, then park. After the park, clear the deadline and return.
- Doing both under the mutex leaves no window for a lost wake-up: an edge either comes before `wait` takes the mutex (the flag is seen) or after (the waiter is seen).
- The deadline sweep (`sweep_io_deadlines`, `poll.rs:357`), `wake_all_io` and `reset` already take the waiter under the mutex; they keep working. A wake from them makes the caller retry its `recv`, which is correct.
- This takes `recv` from 3.0 to 2.0 per request.

**Change 2: do not read a socket that was just drained.**
- With edge-triggered `epoll`, a `recv` that returns fewer bytes than asked has emptied the socket, and any data arriving later produces a new edge (`epoll(7)`).
- Keep a "drained" flag on the plain-socket path of `Socket` (`http.rs`, `Socket::read`). Set it after a short read or `EAGAIN`. When set, `read` calls `wait` first and clears the flag when `wait` returns.
- If the next request arrived in the meantime, its edge set the `ready` flag, so `wait` returns at once and the cost is one atomic swap.
- Never set it after a read of 0 bytes or an error. Leave the TLS path (`read_tls`) as it is: `rustls` buffers records, and "short" means something else there.
- This takes `recv` to 1.0 per request, none failing. WebSocket connections use the same `Socket::read` and gain too.

**Optional:** replace the mutex and flag with one atomic state word (empty, ready, or a waiter pointer). Only if `IoSource::wait` still shows above 2% afterwards.

**Tests:** the HTTP and WebSocket suites; `tests/stress.sh`; new tests for a request that arrives in two segments with a pause between them, two pipelined requests in one segment, a client that closes straight after its request, and an idle connection reaching its timeout.

### 2.2 The clock

- Add `poll::now_coarse() -> u64`: nanoseconds from `clock_gettime(CLOCK_MONOTONIC_COARSE)`, on the same scale as the existing `nanos()`. Its resolution is the kernel tick (1 ms to 4 ms), which is ample for 60 s timeouts swept every 250 ms.
- Pass deadlines as `u64` nanoseconds, or pass the timeout and let `wait` compute the deadline only when it is about to park. `write_all` (`http.rs:359`) almost never parks, so it should read no clock at all.
- `http_date` (`http.rs:788`): replace the global `Mutex<(u64, String)>` and the per-response `String` clone with a per-thread cache of the 29 formatted bytes, keyed by the second from `clock_gettime(CLOCK_REALTIME_COARSE)`, written straight into the output buffer.
- A task can move between threads at a park, so do not hold a reference into thread-local storage across one. Reading the cache and copying 29 bytes in one call is safe.

**Count:** `clock_gettime` per request from 5.0 to 2 or fewer, all coarse.

### 2.3 Parsing without copies

**Now** (`http.rs:589`, `read_request`), for every request:
- `find(&self.buffer, b"\r\n\r\n")` scans byte by byte from the start of the buffer after every read;
- the method, target, decoded path, query and remote address each become a `String`;
- each header becomes two `String`s, the name lower-cased;
- four linear header lookups follow, one of which lower-cases again;
- `self.buffer.drain(..)` moves the remaining bytes down; the body is drained into its own `Vec`.

Then the handler's accessors (`lrt_http_req_method`, `_path`, `_remote`, `_header`, `_body`) copy what they return into collected memory a second time.

**Change:**
- **A read cursor.** `Connection` gets `start` and `scanned` offsets. Consuming a request advances `start`. When `start` reaches the end, both reset to 0; when unread bytes remain and room is short, compact with `copy_within` at the top of `read_request`, never while a `Request` is live.
- **One pass over the head.** Find line ends with libc `memchr` (declare it by hand; the program already links libc). An empty line ends the head. Remember where scanning stopped, so a head that arrives in pieces is not scanned twice.
- **Keep one UTF-8 validation of the head.** It is a few nanoseconds for ASCII, and it preserves today's behaviour for bad bytes; check what that behaviour is and keep it.
- **The request line.** The method becomes an enum for the standard methods with a `&'static str`, plus a range for anything else. The target is a range, split at `?`. Percent-decode the path only if it contains `%`.
- **Headers as ranges.** For each line store the name range and the trimmed value range in an inline array of 32, with a `Vec` for more (keep today's limits and error statuses). While storing, recognise `connection`, `content-length`, `transfer-encoding` and `expect` with a length check and `eq_ignore_ascii_case`.
- **`Request`** holds the ranges and a pointer to the connection. The handle is only valid while the handler runs (`http.rs:900`), which is already the rule.
- **Accessors:**

| Function | Now | After |
|---|---|---|
| `lrt_http_req_method` | Copies into collected memory | Returns a `Str` over static bytes for standard methods |
| `lrt_http_req_path` | `String`, then a collected copy | One collected copy from the buffer |
| `lrt_http_req_header` | Lower-cases the wanted name (allocation), then a copy | Case-insensitive compare, one collected copy on a hit |
| `lrt_http_req_remote` | `String` per request, then a copy | Formatted on demand from the address kept in the connection |
| `lrt_http_req_body` | Drained into a `Vec`, validated, copied | Validated and copied once from the buffer range |

- **Rare paths keep allocating:** chunked request bodies and `Expect: 100-continue` decode into an owned `Vec` as today.
- **WebSocket upgrade:** `take_connection` (`http.rs:531`) hands over only the unread bytes, in a `Vec` of exactly that size (usually empty, so no allocation). See 2.5.
- Other users of `Request::headers()` and `Request::header()` (`ws.rs` for the handshake) move to the new accessors.

**Count:** Rust `malloc` per `/plaintext` request from 9.4 to 1 or fewer; in browser mode it must not depend on the number of headers.

### 2.4 The response

**Now:** `response_bytes` (`http.rs:1004`) builds a new `Vec` per response with `write!`, and `serve_connection` writes it after the handler returns.

**Change:**
- `Connection` gets a reused output buffer. `lrt_http_respond` (`http.rs:983`) writes the head and the body into it through the request's connection pointer.
- The status line comes from a table of byte strings for the common statuses. `Content-Length` uses the hand-written integer formatter that `builder.rs` already has (`format_u64`). The date comes from 2.2.
- **Flush only when about to read.** `serve_connection` writes the output buffer when the input buffer holds no further complete request, when the output passes 16 KiB, or when the connection is closing. Pipelined requests then share one `send`. Responses stay in request order because one task serves the connection.
- If a handler panics, its 500 is appended in order and the connection closes after the flush, as now.
- Shrink the output buffer when its capacity passes 64 KiB, as the input buffer does today.

**Optional:** `writev` for bodies above 64 KiB, straight from the collected string, to avoid one copy. It has to happen inside `lrt_http_respond`, while the caller's frame keeps the body alive, which changes what a later panic in the handler can do. Only with a large-response benchmark that shows the need.

**Tests:** pipelining (three requests in one segment: all answered, in order, in one or two segments); a panic in the second of three pipelined handlers; `HEAD`; a response larger than the flush limit; keep-alive off.

### 2.5 The idle WebSocket memory regression

A bounded look, half a day at most, because this milestone changes the code involved. An idle WebSocket went from 11.3 KiB at milestone 10 to 15.5 KiB (handoff, lead 6).

- **First suspect:** `take_connection` hands the WebSocket the HTTP input buffer with its capacity. `fill` reserves at least 4,096 bytes, and the growth is 4.2 KiB. Item 2.3 already changes this to an exact-size hand-over.
- **Then measure** with `bench/ws_idle/run.py --connections 50000`. If it is back near 11 KiB, done.
- **If not,** look at the WebSocket reader's own buffer and at how deep its read loop's stack goes (each touched stack page is 4 KiB), and stop there. Record what was found either way.

### Exit criteria

| Measure | Before | After |
|---|---|---|
| `recv` per request (failing) | 3.0 (2.0) | 1.0 (about 0) |
| `clock_gettime` per request | 5.0 | 2 or fewer, coarse |
| Rust `malloc` per `/plaintext` request | 9.4 | 1 or fewer |
| `/plaintext`, `/json`, `/users/42` on one CPU | | 12% faster or better in A/B |
| Browser-header mode | Baseline from 1.1 | Within 10% of the bare rate |
| Idle WebSocket | 15.5 KiB | Measured and explained |

Record as D88.

## Milestone 3: Allocation and the collector

### 3.1 A stress mode that exercises the caches

Do this first. Today collector-stress mode bypasses the per-thread caches (`gc.rs`, `lrt_alloc_slow` returns early), so the test suite never collects while objects sit on a free list. Everything else in this milestone changes those lists.

- Add a second mode (a different value of the existing stress variable): the caches stay in use, and the runtime calls `GC_gcollect()` at every refill and at every Nth allocation from a cache.
- Run the `tests/run` programs and `tests/stress.sh` in this mode in addition to the existing modes. An object reclaimed while it waits on a list is then handed out twice, and programs fail visibly.
- Confirm the mode catches such a bug: temporarily break the refill (for example, drop the list's head from the cache) and see tests fail.

### 3.2 Runtime allocations of scanned memory through a per-thread cache

**Now:** `gc::alloc(size)` calls `GC_malloc`. On `/items` that is 46 calls per request: list headers, list buffers, dict headers.

**Change:**
- Give the runtime its own per-thread cache with the same structure as the generated code's `AllocCache`: free lists by granule count, refilled by `GC_malloc_many`, held in a block from `GC_malloc_uncollectable`.
- Keep it separate from the generated code's cache. That one is a thread-local defined in the program's object and read with inline assembly; stable Rust cannot name it. Two caches per thread cost a little memory and no coupling.
- Hold the pointer in a `thread_local!` with a `const` initialiser, and read it only inside a function marked `#[inline(never)]`. A task can move to another thread at any park; a thread-local address that the optimiser reuses across a park would point at another thread's cache. The cache is never used across a park inside that function.
- Check the linked program: the access should be a `%fs`-relative load, not a call to `__tls_get_addr`. If the linker leaves the call in, emit a small accessor function in the generated module that returns the address of the program's own cache slot, and call that instead.
- `gc::alloc` takes the fast path for sizes up to `CACHE_GRANULES` granules. Objects on these lists are scanned memory linked through their first word, so the collector keeps the whole list alive from the cache, exactly as it does today for generated code. Clear the link word on the way out.
- The existing stress mode keeps bypassing the cache.

**Count:** `GC_malloc` per `/items` request from 46 to under 5 (the refills).

### 3.3 Runtime allocations of unscanned memory

**Now:** `gc::alloc_atomic(size)` calls `GC_malloc_atomic`: every string the runtime creates, and every builder buffer. 29 calls per `/items` request, 28 per `POST /orders`.

**Why this is harder.** A free list of unscanned objects linked through their first words is invisible to the collector beyond its head: the collector does not read unscanned memory, so the rest of the list would be reclaimed and handed out again. This is why phase 2 lowered only `lrt_alloc_raw`.

**Design:**
- For unscanned objects the cache holds, per size, an **array of pointers** and a count, not a linked list. The cache block is scanned, so every pointer in it keeps its object alive.
- Refill with `GC_generic_malloc_many(granules * 16, 0, &mut list)` (kind 0 is pointer-free; `GC_malloc_many` is the same call with kind 1). It returns a list linked through first words. Walk it and copy the pointers into the array.
- **The window:** between that call returning and the walk finishing, only the list's head is visible to a collection started by another thread. Bracket the refill and the walk with `GC_disable()` and `GC_enable()`, which stop collections from starting and wait for one in progress.
- Size the arrays for one heap block of objects (4,096 bytes divided by the object size, 256 at most), about 7 KiB per thread in all. If the list is longer than the array, leave the rest: unmarked objects are reclaimed at the next collection.
- A pop is: decrement the count, load the pointer. Nothing is cleared: unscanned memory is handed out uninitialised today as well, and callers fill it.

**This is the riskiest item in the plan.** A mistake is silent memory corruption. Therefore:
- Build it behind the stress mode of 3.1 and run everything in that mode.
- Prototype it and measure before polishing. Keep it only if `POST /orders` and `json_orders` gain 3% or more in A/B and peak memory under load does not grow by more than 5% (`GC_disable` makes other threads grow the heap instead of collecting during the bracket).
- If it is dropped, 3.4 is dropped too, and the record says why.

**Count:** `GC_malloc_atomic` per `POST /orders` request from 28 to under 3.

### 3.4 Pointer-free structs allocated inline

Only if 3.3 is kept. `lower_allocations` (`lang_codegen/src/alloc.rs:40`) redirects `lrt_alloc_raw` with a constant size to the inline fast path and leaves `lrt_alloc_atomic_raw` as a call. Lower it too, popping from the array of 3.3 in the generated code's cache: the cache layout gains the arrays after the existing lists (the lists must stay first; the code generator indexes them), with the layout constants shared between the two crates and checked by a test, as `CACHE_GRANULES` is now.

### 3.5 Allocate the right size the first time

These reduce the bytes allocated, which is what the collector's cost follows.

1. **A size hint for the JSON output builder.** For each type that `json.encode` is compiled for (`json_encode`, `lang_codegen/src/json.rs:719`), emit an internal global holding the length of the last result.
   - Before encoding, reserve that many bytes, capped at 64 KiB. After finishing, store the new length.
   - Use `unordered` atomic loads and stores so that concurrent handlers are not a data race; on x86-64 they compile to plain moves.
   - Round the capacity to a multiple of 16 minus 1: the collector adds a byte to every request, and a round request wastes a granule.
   - A wrong hint costs nothing new: too small falls back to doubling, too large wastes at most the difference, bounded by the cap.
   - Effect on `/items`: 7 buffer allocations and 6 copies become 1 and 0, and the bytes allocated per request fall by about a third.
2. **Comprehensions with a known length.** A comprehension with one `for`, no `if`, over a `range` or a list, produces exactly as many elements as the source has. Sema lowers comprehensions to statements around `lrt_list_new(desc, 0)` (`lang_sema/src/hir.rs:436`). Give the lowering a capacity taken from the source, evaluated once, after the iterable and never by evaluating the iterable's expression twice.
3. **List literals.** `ExprKind::List` (`lang_codegen/src/expr.rs:107`) calls `lrt_list_new(desc, n)` and then pushes each item with a capacity check. Store the items straight into the buffer and set the length once.

**Count:** bytes allocated per `/items` request (from 1.1) down by 30% or more; `GC_malloc` + `GC_malloc_atomic` per `/items` request, with 3.2 and 3.3, from 75 to 10 or fewer.

### 3.6 Parallel marking for batch programs

**Change:**
- In `gc::init`, before `GC_init()`, call `GC_set_markers_count(n)` with `n` the number of CPUs capped at 4. Measure 2, 4 and 6 on `binary_trees` and keep the smallest that gets within a few percent of the best.
- Register a heap-growth callback with `GC_set_on_heap_resize`. It runs with the collector's lock held, so it only sets an atomic flag once the heap passes a threshold, 32 MiB to start with.
- The allocation slow path (`lrt_alloc_slow` and the runtime cache's refill) checks the flag with one load and calls the existing `tasks::allow_threads()`, which is already once-only and is what the first `spawn` calls today.
- Make the threshold an environment variable, with 0 for "never".

**What it trades:** wall time for CPU. `binary_trees` used 45% more CPU for 22% less wall time. Say so in the decision record and the user documentation.

**Check:** `word_count`, `fib`, `dispatch` and `spawn` must not change (their heaps stay under the threshold); `sort`, `sieve` and `json_orders` must not get slower.

### Exit criteria

| Measure | Before | After |
|---|---|---|
| Collector calls per `/items` request | 75 | 10 or fewer (30 or fewer if 3.3 is dropped) |
| Bytes allocated per `/items` request | Baseline from 1.1 | 30% less |
| `/items` | | 6% faster or better in A/B |
| `binary_trees` wall time | 0.65 s | 0.55 s or less |
| Peak memory, all benchmarks | | No more than 5% worse |
| Cache-stress mode | Does not exist | Whole suite passes in it |

Record as D89.

## Milestone 4: Generated code: builders and dicts

### 4.1 Builder pushes inline

**Now:** `push_text` (`lang_codegen/src/expr.rs:979`) emits a call to `lrt_builder_push(b, ptr, len)` for every literal piece of an f-string or of JSON output. The `Builder` record (`data`, `len`, `cap`) is already on the generated function's stack.

**Change:**
- Define an internal always-inline function in each module, in the manner of the allocation fast path (`alloc.rs:80`): load `len` and `cap`; if `len + n > cap`, call `lrt_builder_push` (marked cold); otherwise `memcpy` to `data + len` and store the new length.
- Emit it before the optimisation pipeline, not after as the allocation lowering is, so that the pipeline inlines it and turns each constant-length copy into immediate stores.
- **Merge neighbouring literals at compile time.** Give the JSON encoder (`lang_codegen/src/json.rs`) a pending-literal string: `push_text` appends to it, and it is flushed before any other push and before every change of basic block (branches, loop heads, loop ends). Forgetting a flush puts text in the wrong block; the existing JSON tests catch that, and a new test with optional fields, empty lists and nested structs in every position should make sure.

**Do not** try to help LLVM with `memory(...)` attributes on the builder functions. It was considered and is unsafe once pushes are inline: the builder's buffer is reachable from generated code, so it is neither argument memory nor inaccessible memory, and declaring it so lets LLVM delete the inline stores as dead. The reloads the attributes would save are two first-level-cache loads per call.

**Expected:** 3% to 5% on `/items` and `json_orders`. Drop it if A/B shows under 2%.

### 4.2 Dicts with `str` and integer keys

**Now** (`lang_rt/src/dict.rs`):
- Generated code calls `lrt_dict_find(d, key_ptr)` and `lrt_dict_slot(d, key_ptr)` with the key spilled to the stack.
- `hash()` (`dict.rs:78`) checks on every call whether the keys are `str` by loading the descriptor and comparing a function pointer, then computes `hash_str`, then applies `mix` (a SplitMix64 finaliser) on top of a hash that is already well mixed.
- `hash_bytes` (`strings.rs:82`) reads 8-byte chunks and copies the tail into a zeroed 8-byte buffer with a variable-length copy. For keys of a few bytes, that copy is most of the cost. `seed()` goes through a `OnceLock`.
- `keys_equal` repeats the `str` check for every probe.
- Other key types hash and compare through function pointers.

**Change:**
1. **Entry points chosen by the compiler.** The compiler knows the key type at every call site. Add `lrt_dict_find_str(d, ptr, len)`, `lrt_dict_slot_str`, `lrt_dict_find_int(d, key)`, `lrt_dict_slot_int`, and the same for `remove` and `contains` if they are separate. Each has its hash and its comparison written inline, with no check of the descriptor and no spill. The generic functions stay for other key types and for whole-dict operations.
2. **A hash path for short strings.** For lengths up to 16, read two overlapping words (first 8 and last 8 bytes for 8 to 16; two overlapping 4-byte reads for 4 to 7; three byte reads for 1 to 3) and do one multiply-fold with the seed and the length. No loop, no copy. Longer strings keep the chunk loop.
3. **No second mix for strings.** Use the string hash directly (with the low bit set, as now). Keep `mix` for integers, inline.
4. **The seed in a plain atomic,** set once in runtime start-up and read with a relaxed load.
5. **The generic path** (`lrt_dict_find`, `lrt_dict_slot` for `str` keys reached through generic code, `lrt_dict_copy`, `lrt_dict_eq`) must produce the same hashes as the specialised entry points: one dict can be reached through both. Share the functions.
6. **Optional: hash bits in the index slot.** The index holds an `i64` entry number per slot. Packing 32 bits of the hash beside a 32-bit entry number lets a probe reject a collision without touching the entry. Do it only if `probe` is still above 8% of `word_count` after items 1 to 4; it changes growth, removal and copying.

**What cannot change:** insertion order on iteration; that hashes are seeded per process; that growing the table reuses the hash stored in each entry.

**Tests:** the dict programs in `tests/run`; a Rust unit test that runs random operations against `std::collections::HashMap` plus an insertion-order list, for `str` keys of every length from 0 to 40 including non-ASCII, and for integer keys including negative and consecutive ones; a test that a dict filled through a generic function is read correctly through a specialised entry point and the other way round.

**Expected:** 10% to 20% on `word_count`; a little on `/users/42` and `POST /orders`, which use small `str`-keyed dicts.

### 4.3 Small items

Each is independent. Read the IR before and after, and keep only what measures 2% or more on some benchmark.

- **Indexing with one compare on the fast path.** `list[i]` (`expr.rs:526`) computes `i < 0 ? i + len : i` and then does an unsigned bounds check. Instead: if `i` is below `len` as an unsigned number, index directly; otherwise go to a cold block that handles negative indexes and the panic. One compare and one predictable branch replace a compare, a select and a compare. Judge it on `sieve`.
- **`bool` elements loaded as bytes.** `list[bool]` elements are loaded as `i1`. If the IR of `sieve`'s counting loop shows that this blocks vectorisation, load `i8` and compare with zero.
- **A `--cpu` flag.** The target machine is created with the `generic` CPU and no features (`lang_codegen/src/lib.rs:115`). Add `--cpu <name>` to `build`, passed through to LLVM, with `native` allowed. The default stays `generic`, and the help text says that the result may not run on older processors.

### Exit criteria

| Measure | Before | After |
|---|---|---|
| `word_count` | 0.65 s | 10% faster or better in A/B |
| `/items` | | 3% faster or better in A/B, or 4.1 is dropped |
| `Dict::hash` + `Dict::probe` share of `word_count` | 44% | 25% or less |

Record as D90.

## Milestone 5: Close-out

1. **Full runs,** on an otherwise idle machine:

```bash
python3 bench/run.py --runs 3
```

```bash
python3 bench/http/run.py --connections 64,256 --runs 3 --seconds 5
```

```bash
python3 bench/http/one_core.py
```

```bash
python3 bench/ws_idle/run.py --connections 50000
```

   Add the browser-header run and the counts table from milestone 1.
2. **A regression check.** `bench/run.py --check` compares a run against recorded results and fails beyond a tolerance of 15% on time or 10% on peak memory. It was planned in [09](09-performance-plan.md) and never built. `counts.py --check` does the same for the per-request counts, with no tolerance beyond rounding: these are the cheapest guard against this work being undone.
3. **Documents:**
   - [web-performance.md](web-performance.md): all tables, the "in short" figures, the date.
   - `bench/README.md`: results, and the new tools.
   - [08-decisions-and-risks.md](08-decisions-and-risks.md): D87 to D90 complete.
   - [optimization-handoff.md](optimization-handoff.md): mark leads 1 to 4, 6 and 7 as done or measured; state what remains.
   - [04-runtime-and-memory.md](04-runtime-and-memory.md) and [03-compiler-architecture.md](03-compiler-architecture.md): the two allocation caches, the readiness protocol, the stack-allocated JSON reader, the dict entry points, parallel marking and its threshold.
   - [building.md](building.md): new environment variables and flags.
   - `README.md`: the headline figures.
4. **Dead code.** Remove what the milestones left behind (`response_bytes`, `find`, the old `float_repr` internals outside tests), and any item that was tried and dropped.
5. **A closing statement** in the handoff: the final profile of each endpoint, the share that is kernel time, and the list in section 5 as the answer to "what would it take to go further".

## 5. Not in this pass

| Idea | Why not |
|---|---|
| Value types in collections; dropping the struct header | Language and ABI design questions (handoff, lead 5). They decide memory use and `sort`, not the request path |
| Small-string optimisation | Changes the representation every string operation relies on. `word_count` gets most of its gain from 4.2 without it |
| A precise or moving collector (MMTk) | Weeks of work and a download; decided in D86. After milestone 3 the collector's share is driven by bytes allocated, which this plan cuts |
| `io_uring` | The remaining system-call cost is the kernel's TCP work inside `recv` and `send` on loopback, which `io_uring` does not remove. After 2.1 there is one of each per request, the same as with `io_uring` without batching |
| Hot helpers in C compiled to bitcode | A new build mechanism for what 4.1 and 4.2 get with hand-written IR and specialised entry points. Hashing a constant key at compile time is impossible while the seed is per process |
| `memory(...)` attributes on builder, list and dict runtime functions | Unsafe for builders once pushes are inline (4.1), and the saving is two cached loads per call |
| `sort`'s string comparisons | Cache misses on string bytes during the comparison; already 2.2x Go. A fix needs key prefixes stored in the sort buffer: a new sort implementation for one benchmark |
| Scheduler tuning | No evidence of a bottleneck: 0.02 to 0.15 `epoll_wait` per request, and phase 3 found spinning workers harmful |
| `main` as a fiber | Affects only programs whose `main` competes with workers; not a throughput item |
| TLS, HTTP/2, a database benchmark | Not measured today. The first step would be a benchmark, not an optimisation |

**What going further would need.** After this plan, a small request is about 0.75 µs of user-space time and 1.5 µs of kernel time. The remaining user-space cost is spread thinly over the handler, the router, the scheduler and the collector. Further gains need one of: fewer kernel crossings per request (only possible with pipelining or batching across connections), a collector that allocates by bumping a pointer and does not sweep (a precise collector), or objects stored inline in collections (value types).

## 6. Expected outcome

Estimates, compounded from the per-milestone gains; the ranges reflect how rough they are.

| Measure | Now | After | Against Go now | Against Go after |
|---|---|---|---|---|
| `/plaintext`, one CPU | 2.6 µs | about 2.2 µs | 2.05x | about 2.4x |
| `/items`, one CPU | 9.6 µs | 6.8 to 7.4 µs | 1.14x | 1.45x to 1.6x |
| `/users/42`, one CPU | 3.2 µs | about 2.7 µs | 1.85x | about 2.2x |
| `POST /orders`, one CPU | 4.6 µs | 3.6 to 3.9 µs | 1.99x | 2.3x to 2.5x |
| `json_orders` | 0.52 s | 0.37 to 0.41 s | 1.15x | 1.45x to 1.6x |
| `word_count` | 0.65 s | 0.52 to 0.58 s | 1.20x | 1.35x to 1.5x |
| `binary_trees` | 0.65 s | 0.50 to 0.55 s wall | 1.29x | 1.5x to 1.65x |
| Requests with browser headers | Not measured | Within 10% of bare | | |

## 7. Risks

| Risk | Where | Guard |
|---|---|---|
| A lost wake-up leaves a connection parked for ever | 2.1 | Flag and waiter handled under one mutex; segment-split and idle-timeout tests; `tests/stress.sh` |
| A missed edge after a short read | 2.1 | Only on plain sockets, never after 0 bytes or an error; the `recv` count shows it working; the split-request test |
| A free-list object reclaimed while cached | 3.2, 3.3 | The cache-stress mode of 3.1, built first and proven to catch a planted bug |
| A thread-local address reused across a park | 3.2 | Access only inside an `#[inline(never)]` function that never parks |
| Float text changes in some rare case | 1.3 | Old implementation kept in tests; two million random comparisons and the edge list |
| JSON error paths change | 1.5 | Existing message tests; new deep-nesting test |
| Literal text flushed into the wrong block | 4.1 | Flush at every block change; a test with optional and empty values in every position |
| A dict reached through two entry points disagrees on hashes | 4.2 | Shared hash functions; the cross-path test |
| Timing noise hides or invents an effect | all | Counts as the primary criterion; interleaved A/B for timings |
| Peak memory grows | 3.3, 3.5, 3.6 | Checked at every exit; 5% limit |

## Appendix A: Tools used for the evidence

These were scratch files during planning and are not in the repository. Milestone 1 adds the first to `bench/profile/`.

**The call counter.** Build with `cc -O2 -shared -fPIC count.c -o count.so -ldl`, run the server with `LD_PRELOAD=./count.so`, stop it with `SIGTERM`, and divide the totals it prints by the request count. The shim itself slows allocation-heavy endpoints (it wraps `malloc`), so use it for counts only, never for rates.

```c
#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdatomic.h>
#include <stdio.h>
#include <sys/epoll.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <sys/uio.h>
#include <time.h>
#include <unistd.h>

extern void *__libc_malloc(size_t);
extern void *__libc_calloc(size_t, size_t);
extern void *__libc_realloc(void *, size_t);
extern void __libc_free(void *);

#define COUNTER(name) static atomic_long n_##name
COUNTER(recv); COUNTER(recv_empty); COUNTER(send); COUNTER(writev); COUNTER(epoll); COUNTER(epoll_empty);
COUNTER(read); COUNTER(write); COUNTER(clock); COUNTER(malloc); COUNTER(free);
COUNTER(gc_malloc); COUNTER(gc_atomic); COUNTER(gc_many);

#define NEXT(ret, name, ...) static ret (*real)(__VA_ARGS__); if (!real) real = dlsym(RTLD_NEXT, name)

void *malloc(size_t n) { n_malloc++; return __libc_malloc(n); }
void *calloc(size_t a, size_t b) { n_malloc++; return __libc_calloc(a, b); }
void *realloc(void *p, size_t n) { n_malloc++; return __libc_realloc(p, n); }
void free(void *p) { if (p) n_free++; __libc_free(p); }

ssize_t recv(int fd, void *b, size_t n, int f) {
  NEXT(ssize_t, "recv", int, void *, size_t, int);
  ssize_t r = real(fd, b, n, f); n_recv++; if (r < 0) n_recv_empty++; return r;
}
ssize_t send(int fd, const void *b, size_t n, int f) {
  NEXT(ssize_t, "send", int, const void *, size_t, int); n_send++; return real(fd, b, n, f);
}
ssize_t writev(int fd, const struct iovec *v, int c) {
  NEXT(ssize_t, "writev", int, const struct iovec *, int); n_writev++; return real(fd, v, c);
}
int epoll_wait(int e, struct epoll_event *ev, int m, int t) {
  NEXT(int, "epoll_wait", int, struct epoll_event *, int, int);
  int r = real(e, ev, m, t); n_epoll++; if (r <= 0) n_epoll_empty++; return r;
}
int clock_gettime(clockid_t c, struct timespec *t) {
  NEXT(int, "clock_gettime", clockid_t, struct timespec *); n_clock++; return real(c, t);
}
ssize_t write(int fd, const void *b, size_t n) {
  NEXT(ssize_t, "write", int, const void *, size_t); n_write++; return real(fd, b, n);
}
ssize_t read(int fd, void *b, size_t n) {
  NEXT(ssize_t, "read", int, void *, size_t); n_read++; return real(fd, b, n);
}
void *GC_malloc(size_t n) { NEXT(void *, "GC_malloc", size_t); n_gc_malloc++; return real(n); }
void *GC_malloc_atomic(size_t n) { NEXT(void *, "GC_malloc_atomic", size_t); n_gc_atomic++; return real(n); }
void *GC_malloc_many(size_t n) { NEXT(void *, "GC_malloc_many", size_t); n_gc_many++; return real(n); }
void GC_generic_malloc_many(size_t n, int k, void **r) {
  NEXT(void, "GC_generic_malloc_many", size_t, int, void **); n_gc_many++; real(n, k, r);
}

__attribute__((destructor)) static void report(void) {
  fprintf(stderr,
          "COUNT recv=%ld recv_empty=%ld send=%ld writev=%ld epoll_wait=%ld epoll_empty=%ld read=%ld write=%ld "
          "clock_gettime=%ld malloc=%ld free=%ld GC_malloc=%ld GC_malloc_atomic=%ld GC_malloc_many=%ld\n",
          (long)n_recv, (long)n_recv_empty, (long)n_send, (long)n_writev, (long)n_epoll, (long)n_epoll_empty,
          (long)n_read, (long)n_write, (long)n_clock, (long)n_malloc, (long)n_free, (long)n_gc_malloc,
          (long)n_gc_atomic, (long)n_gc_many);
}
```

**Parallel marking from the start,** for the measurement in section 2.4. Build as a shared object and preload it:

```c
extern void GC_init(void);
extern void GC_allow_register_threads(void);
__attribute__((constructor)) static void on(void) { GC_init(); GC_allow_register_threads(); }
```

**Profiles** are from `bench/profile/sampler.c` and `bench/profile/report.py` as documented in `bench/README.md`, with the server under `taskset -c 0-2,6-8` and `bench/http/load.go` under `taskset -c 3-5,9-11`, 256 connections for 8 s.
