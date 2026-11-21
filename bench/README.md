# Benchmarks

Each benchmark exists as a Viper program (`.vi`), a Go program (`.go`) and a CPython program (`.py`), all printing the same output. `run.py` builds the Viper version with `--release` and the Go version with `go build`, runs all three, checks that the outputs match, and prints a table. Go is found on `PATH` or in `~/.local/go/bin`; without it, the Go column is left out.

```bash
cargo build --release --workspace
```

```bash
python3 bench/run.py --runs 5
```

| Benchmark | What it measures |
|---|---|
| `dispatch` | Dynamic dispatch: 10 million method calls through trait objects (Go interfaces, Python methods) |
| `fib` | Function calls and integer arithmetic: naive recursive `fib(35)` |
| `sieve` | List indexing and loops: a sieve of Eratosthenes over 20 million booleans, grown by appending |
| `binary_trees` | Allocation and garbage collection: the classic binary-trees benchmark at depth 18 |
| `word_count` | Strings and dicts: split, lower-case and count 20.5 million words |
| `json_orders` | Typed JSON: 20,000 orders with five line items each, encoded and decoded ten times (Go: `encoding/json` with struct tags; Python: `json` plus building dataclasses) |
| `spawn` | Task creation: 200,000 tasks spawned and joined, 10,000 at a time (Go: goroutines; Python: a thread pool) |
| `sort` | Sorting with a function: a million records sorted by a key closure, then copied and sorted by a tuple key (Go: `sort.SliceStable` with a comparator closure) |

The Go versions are written the way a Go programmer would write them. Where that changes the work done, the comment in the file says so. For example, `counts[key] = counts[key] + 1` is one map access in Go but two lookups in the other versions.

## HTTP

`bench/http/` holds the same small service three times: `server.vi`, `server.go` (`net/http`) and `server.py` (the standard library's `ThreadingHTTPServer`, since no Python web framework is installed and installing one is a download). Each answers three endpoints:

| Endpoint | What it does |
|---|---|
| `/plaintext` | Returns a fixed string |
| `/json` | Encodes a small struct to JSON |
| `/items` | Builds and encodes a list of 20 records, each with a formatted name and a list of tags: 1.1 KB of JSON |

`load.go` is a closed-loop load generator: N connections, each sending the next request as soon as the previous response arrives, over keep-alive. It runs on the same machine as the server.

```bash
python3 bench/http/run.py --seconds 5 --connections 64
```

`one_core.py` runs the same endpoints with each server on a single CPU (`VIPER_WORKERS=1`, `GOMAXPROCS=1`) and prints the CPU time each request costs:

```bash
python3 bench/http/one_core.py
```

The summary for readers deciding whether to use the language is [docs/web-performance.md](../docs/web-performance.md).

`--browser` (for `run.py` and `load.go`) adds the dozen headers a browser sends, about 600 bytes, to every request. Without it, requests carry only `Host`, which understates the cost of parsing headers.

## Measuring a change

Small effects (a few percent) are hidden by the machine's drift of up to 10% between runs a few minutes apart. Three tools make them visible (docs/10, milestone 1):

- **`counts.py`** runs the server under `profile/count.c`, an `LD_PRELOAD` shim, and prints exact counts per request for each endpoint: `recv` calls (and how many found nothing to read), `send`, `epoll_wait`, clock reads, Rust heap allocations, collector calls and bytes allocated. Counts do not drift, so they show whether a change did what it was meant to.

```bash
python3 bench/http/counts.py [--browser] [--server EXECUTABLE]
```

- **`ab.py`** runs two builds of the same program interleaved, in alternating order, and prints each side's median and range. Build the "before" side against a saved runtime with `VIPER_RUNTIME=saved/liblang_rt.a`.

```bash
python3 bench/ab.py before after --rounds 5
```

```bash
python3 bench/ab.py before_server after_server --server --one-core --endpoints /items,/plaintext
```

- **`VIPER_ALLOC_STATS=1`** makes any compiled program print the bytes it allocated, its number of collections and its heap size when it exits.

`profile/` has a sampling profiler for machines without `perf` (`sampler.c`, `report.py`; usage in `sampler.c`).

## Regression checks

Two checks guard what the final optimisation pass gained (docs/10, milestone 5). Run them after a change that could affect performance, on an otherwise idle machine:

```bash
python3 bench/run.py --check
```

```bash
python3 bench/http/counts.py --check
```

```bash
python3 bench/http/counts.py --browser --check
```

- **`run.py --check`** runs each program's Viper and Go versions (not CPython) and compares Viper's time *relative to Go's* with `baseline.json`, which cancels most of the machine's drift. It fails on a program more than 15% slower (and at least 5 ms) or using more than 10% more memory.
- **`counts.py --check`** compares the calls and allocations per request with `http/counts.json` (and `counts_browser.json`): `recv`, `send`, clock reads, Rust heap allocations, collector calls and bytes allocated. These do not drift, so the tolerance is small: 0.1 per request or 5%. `epoll_wait` and free-list refills depend on timing and are left out.
- After an intended change, record new baselines with `--record` in place of `--check`, and commit them.

## Idle WebSockets and task creation

`ws_idle/` holds a WebSocket echo server (`server.vi`) and a Go client (`client.go`) that opens N connections from several loopback addresses, holds them idle, then checks that each still echoes. `run.py` measures the server's resident memory before and after.

```bash
python3 bench/ws_idle/run.py --connections 50000
```

`spawn` (in the main suite) starts 10,000 tasks that each add two numbers, joins them, twenty times. Go uses goroutines with a result channel each; Python a pool of 64 threads.

## Results

### 2026-11-19, final optimisation pass

After milestones 1 to 4 of the [final optimisation plan](../docs/10-final-optimization-plan.md) (docs/08, D87 to D90). Best of 3 runs. This run was about 7% slower than the A/B runs in the decision records (Go's `json_orders` took 0.710 s here against 0.664 s for phase 5): compare the ratios, not the times.

| Benchmark | Viper | Go 1.27.4 | CPython 3.13.5 | Viper vs Go | Viper vs CPython | Memory: Viper / Go / CPython |
|---|---|---|---|---|---|---|
| binary_trees | 0.646 s | 0.946 s | 24.000 s | 1.46x faster | 37.1x faster | 65 / 40 / 56 MB |
| dispatch | 0.010 s | 0.033 s | 0.617 s | 3.39x faster | 63.0x faster | 13 / 13 / 13 MB |
| fib | 0.022 s | 0.037 s | 0.686 s | 1.72x faster | 31.7x faster | 13 / 13 / 13 MB |
| json_orders | 0.239 s | 0.710 s | 3.714 s | 2.97x faster | 15.5x faster | 65 / 60 / 129 MB |
| sieve | 0.055 s | 0.060 s | 2.502 s | 1.09x faster | 45.4x faster | 60 / 56 / 161 MB |
| sort | 0.587 s | 1.212 s | 2.192 s | 2.07x faster | 3.7x faster | 149 / 75 / 302 MB |
| spawn | 0.040 s | 0.046 s | 1.292 s | 1.14x faster | 32.4x faster | 14 / 18 / 47 MB |
| word_count | 0.387 s | 0.829 s | 2.437 s | 2.14x faster | 6.3x faster | 13 / 13 / 13 MB |

Against phase 5 (the entry below): `json_orders` went from 1.10x to 2.97x Go's speed, `word_count` from 1.22x to 2.14x, `binary_trees` from 1.27x to 1.46x, `spawn` from level to 1.14x, `sieve` from 1.05x to 1.09x. `sort` (2.24x to 2.07x) and `fib` (1.81x to 1.72x) are within this machine's run-to-run variation; their interleaved A/B runs showed no change. Memory is unchanged except `json_orders` (71 to 65 MB) and `spawn` (15 to 14 MB).

HTTP at 256 connections, requests per second (median / p99 latency in ms; peak memory), median of 3 runs of 5 s, pinned:

| Endpoint | Viper | Go 1.27.4 | Python 3.13.5 | Viper vs Go |
|---|---|---|---|---|
| `/plaintext` | 959,525 (0.21 / 0.85; 27 MB) | 502,591 (0.37 / 1.97; 23 MB) | 27,964 | 1.91x faster |
| `/json` | 965,126 (0.21 / 0.85; 28 MB) | 488,599 (0.38 / 2.03; 23 MB) | 24,681 | 1.98x faster |
| `/items` | 527,617 (0.38 / 1.14; 27 MB) | 235,700 (0.77 / 5.23; 24 MB) | 16,907 | 2.24x faster |
| `/users/42` | 853,844 (0.24 / 1.01; 26 MB) | 461,751 (0.41 / 2.04; 25 MB) | 23,397 | 1.85x faster |
| `POST /orders` | 857,771 (0.24 / 0.90; 20 MB) | 275,525 (0.64 / 4.31; 24 MB) | 18,272 | 3.11x faster |

One CPU per server, 64 connections, best of 3 runs of 4 s (`http/one_core.py`):

| Endpoint | Viper | Go 1.27.4 | Viper vs Go | CPU per request: Viper / Go |
|---|---|---|---|---|
| `/plaintext` | 458,331 | 189,260 | 2.42x | 2.2 / 5.3 µs |
| `/json` | 526,076 | 189,382 | 2.78x | 1.9 / 5.3 µs |
| `/items` | 278,452 | 97,875 | 2.84x | 3.6 / 10.2 µs |
| `/users/42` | 433,433 | 174,870 | 2.48x | 2.3 / 5.7 µs |
| `POST /orders` | 348,768 | 116,596 | 2.99x | 2.9 / 8.6 µs |

What changed, against phase 5:

- **`/items`**, the weakest row, went from level with Go to 2.24x at 256 connections and 2.84x on one CPU (9.6 to 3.6 µs per request): no Rust heap allocation in formatting or JSON (D87), runtime allocations from per-thread lists (D89), and inline builder pushes (D90).
- **`POST /orders`** went from 2.12x to 3.11x Go at 256 connections.
- **The small endpoints** cost 1.9 to 2.3 µs on one CPU instead of 2.6 to 3.2: one `recv` and two coarse clock reads per request, and no allocation in parsing (D88). At 256 connections on six CPUs their rates barely moved, because the load generator and the kernel on the other six CPUs set them; the server has idle CPU (D88).
- `/plaintext` measured below `/json` on one CPU in this run, which interleaved A/B runs do not show (both about 520,000): take it as this run's variation.
- Startup 3 ms, idle memory 3.5 MB, executable 1.3 MB (7.6 MB before LTO, 5.2 MB before the runtime's debug information was left out, D95).
- Idle WebSockets: 18.9 KiB each with 50,000 open, up from 15.5 KiB. LTO puts one more stack page under each parked connection (D88).

Per request (`http/counts.py`; the same with a dozen browser headers): 1 `recv` (none failing), 1 `send`, 2 coarse clock reads, at most 1.5 Rust heap allocations, and on `/items` 22 collector calls and 5.3 KB allocated.

### 2026-11-19, performance plan phase 5

Phase 5 of [docs/09](../docs/09-performance-plan.md) measured the collector's costs and decided to stay on Boehm, with the heap-growth divisor's default raised from 1 to 2 (docs/08, D86). Best of 3 runs. The machine was about 10% slower than for phase 4: compare the ratios, not the times.

| Benchmark | Viper | Go 1.27.4 | CPython 3.13.5 | Viper vs Go | Viper vs CPython | Memory: Viper / Go / CPython |
|---|---|---|---|---|---|---|
| binary_trees | 0.736 s | 0.938 s | 21.833 s | 1.27x faster | 29.7x faster | 65 / 40 / 56 MB |
| dispatch | 0.011 s | 0.032 s | 0.636 s | 3.05x faster | 60.0x faster | 13 / 13 / 13 MB |
| fib | 0.021 s | 0.038 s | 0.678 s | 1.81x faster | 31.8x faster | 13 / 13 / 13 MB |
| json_orders | 0.606 s | 0.664 s | 3.642 s | 1.10x faster | 6.0x faster | 71 / 59 / 130 MB |
| sieve | 0.061 s | 0.064 s | 2.340 s | 1.05x faster | 38.5x faster | 60 / 55 / 161 MB |
| sort | 0.537 s | 1.203 s | 2.169 s | 2.24x faster | 4.0x faster | 149 / 81 / 302 MB |
| spawn | 0.047 s | 0.047 s | 1.280 s | 1.00x faster | 27.1x faster | 15 / 14 / 47 MB |
| word_count | 0.674 s | 0.825 s | 2.447 s | 1.22x faster | 3.6x faster | 13 / 13 / 13 MB |

HTTP at 256 connections, requests per second (median / p99 latency in ms; peak memory), median of 3 runs of 5 s, pinned:

| Endpoint | Viper | Go 1.27.4 | Python 3.13.5 | Viper vs Go |
|---|---|---|---|---|
| `/plaintext` | 1,010,397 (0.21 / 0.79; 27 MB) | 528,241 (0.36 / 1.77; 25 MB) | 27,269 | 1.91x faster |
| `/json` | 901,450 (0.23 / 0.98; 20 MB) | 495,903 (0.38 / 1.93; 24 MB) | 25,457 | 1.82x faster |
| `/items` | 229,082 (0.99 / 2.46; 28 MB) | 227,851 (0.78 / 5.48; 22 MB) | 17,018 | 1.01x faster |
| `/users/42` | 848,079 (0.25 / 0.92; 26 MB) | 475,075 (0.40 / 1.90; 24 MB) | 23,956 | 1.79x faster |
| `POST /orders` | 615,022 (0.36 / 1.04; 20 MB) | 289,549 (0.65 / 3.60; 24 MB) | 19,170 | 2.12x faster |

What changed:

- **`binary_trees`** peaks at 65 MB instead of 90 MB, 1.6x Go's memory instead of 2.1x. In an A/B run on the same build it took about 5% longer, and it is still 1.27x faster than Go.
- **`json_orders`** went from 79 to 71 MB, and **`spawn`** from 19 to 15 MB.
- **`/items`** is level with Go here, against 1.08x in phase 4. The divisor accounts for about 1.5 points of that in A/B runs (D86); the rest is this slower run.
- Measured for the decision (D86):
  - collection is about 2% of `/items`' CPU;
  - a server under a minute of sustained load stays at 24 MB throughout;
  - the remaining memory gap with Go comes from object layout, not from collection.

### 2026-11-19, web performance report

The measurements behind [docs/web-performance.md](../docs/web-performance.md), on the phase 4 build. HTTP at 64 and 256 connections is in that report.

One CPU per server, 64 connections, best of 3 runs of 4 s (`one_core.py`):

| Endpoint | Viper | Go | Viper vs Go | CPU per request: Viper / Go |
|---|---|---|---|---|
| `/plaintext` | 382,310 | 186,777 | 2.05x | 2.6 / 5.4 µs |
| `/json` | 377,275 | 179,428 | 2.10x | 2.7 / 5.6 µs |
| `/items` | 104,364 | 91,637 | 1.14x | 9.6 / 10.9 µs |
| `/users/42` | 315,783 | 170,838 | 1.85x | 3.2 / 5.9 µs |
| `POST /orders` | 219,666 | 110,386 | 1.99x | 4.6 / 9.1 µs |

Idle WebSockets: 50,000 connections opened in 2.2 s, all still echoing afterwards; the server grew from 4.2 MB to 763 MB, **15.5 KiB per connection**. The phase 2 build, measured the same way, used 16.5 KiB; milestone 10's figure was 11.3 KiB.

### 2026-11-19, performance plan phase 4

Phase 4 of [docs/09](../docs/09-performance-plan.md): devirtualised trait calls, `str` keys hashed and compared directly in dicts, cheaper `split` and lower-casing, and string concatenation chains allocated once (docs/08, D85). Best of 3 runs.

| Benchmark | Viper | Go 1.27.4 | CPython 3.13.5 | Viper vs Go | Viper vs CPython | Memory: Viper / Go / CPython |
|---|---|---|---|---|---|---|
| binary_trees | 0.646 s | 0.834 s | 20.149 s | 1.29x faster | 31.2x faster | 90 / 42 / 56 MB |
| dispatch | 0.009 s | 0.031 s | 0.591 s | 3.38x faster | 64.4x faster | 13 / 13 / 13 MB |
| fib | 0.021 s | 0.036 s | 0.644 s | 1.73x faster | 30.7x faster | 13 / 13 / 13 MB |
| json_orders | 0.523 s | 0.602 s | 3.390 s | 1.15x faster | 6.5x faster | 79 / 67 / 130 MB |
| sieve | 0.051 s | 0.052 s | 2.277 s | 1.03x faster | 44.6x faster | 61 / 64 / 161 MB |
| sort | 0.477 s | 1.132 s | 2.094 s | 2.37x faster | 4.4x faster | 148 / 79 / 302 MB |
| spawn | 0.040 s | 0.042 s | 0.993 s | 1.03x faster | 24.7x faster | 16 / 14 / 47 MB |
| word_count | 0.648 s | 0.778 s | 2.331 s | 1.20x faster | 3.6x faster | 13 / 13 / 13 MB |

HTTP at 256 connections, requests per second (median / p99 latency in ms; peak memory), median of 3 runs of 5 s, pinned:

| Endpoint | Viper | Go 1.27.4 | Python 3.13.5 | Viper vs Go |
|---|---|---|---|---|
| `/plaintext` | 1,131,830 (0.19 / 0.68; 27 MB) | 572,537 (0.33 / 1.58; 23 MB) | 30,074 | 1.98x faster |
| `/json` | 1,122,128 (0.19 / 0.68; 30 MB) | 549,237 (0.35 / 1.71; 24 MB) | 27,921 | 2.04x faster |
| `/items` | 279,053 (0.82 / 1.76; 24 MB) | 259,527 (0.73 / 3.97; 22 MB) | 18,883 | 1.08x faster |
| `/users/42` | 965,229 (0.22 / 0.86; 32 MB) | 507,904 (0.38 / 1.78; 25 MB) | 25,836 | 1.90x faster |
| `POST /orders` | 688,747 (0.33 / 0.99; 24 MB) | 306,248 (0.60 / 3.60; 26 MB) | 20,212 | 2.25x faster |

What changed:

- **`dispatch`** went from 0.052 s to 0.009 s. With three types implementing `Shape`, each call compares the vtable and calls the method directly. LLVM then inlines the three `area` methods, and the running total stays in a register.
- **`word_count`** went from 0.72 s to 0.65 s. Dicts with `str` keys hash and compare without calls through the descriptor, `split` allocates its list once at its size, and lower-casing checks ASCII text byte by byte.
- The HTTP rows moved within run-to-run noise, slightly up; the handlers make no trait-object calls.
- Every benchmark and every endpoint is now at or ahead of Go. `binary_trees` and `sort` still use about twice Go's memory (phase 5).

### 2026-11-19, performance plan phase 3

Phase 3 of [docs/09](../docs/09-performance-plan.md): no collector lock on the context switch, cheaper task creation, a router that does not allocate per route tried, and a 4 MiB floor on allocation between collections (docs/08, D84). Best of 3 runs.

| Benchmark | Viper | Go 1.27.4 | CPython 3.13.5 | Viper vs Go | Viper vs CPython | Memory: Viper / Go / CPython |
|---|---|---|---|---|---|---|
| binary_trees | 0.640 s | 0.839 s | 21.627 s | 1.31x faster | 33.8x faster | 90 / 42 / 57 MB |
| dispatch | 0.052 s | 0.030 s | 0.594 s | 1.73x the time | 11.3x faster | 13 / 13 / 13 MB |
| fib | 0.021 s | 0.036 s | 0.651 s | 1.67x faster | 30.6x faster | 13 / 13 / 13 MB |
| json_orders | 0.514 s | 0.604 s | 3.371 s | 1.17x faster | 6.6x faster | 79 / 59 / 130 MB |
| sieve | 0.052 s | 0.052 s | 2.259 s | 1.02x faster | 43.9x faster | 61 / 55 / 161 MB |
| sort | 0.484 s | 1.148 s | 2.101 s | 2.37x faster | 4.3x faster | 145 / 75 / 302 MB |
| spawn | 0.041 s | 0.042 s | 0.983 s | 1.03x faster | 23.8x faster | 19 / 14 / 47 MB |
| word_count | 0.724 s | 0.771 s | 2.414 s | 1.07x faster | 3.3x faster | 13 / 13 / 13 MB |

HTTP at 256 connections, requests per second (median / p99 latency in ms; peak memory), median of 3 runs of 5 s, pinned:

| Endpoint | Viper | Go 1.27.4 | Python 3.13.5 | Viper vs Go |
|---|---|---|---|---|
| `/plaintext` | 1,102,187 (0.19 / 0.71; 25 MB) | 563,294 (0.34 / 1.65; 24 MB) | 29,611 | 1.96x faster |
| `/json` | 1,037,666 (0.20 / 0.76; 19 MB) | 526,296 (0.36 / 1.86; 23 MB) | 27,654 | 1.97x faster |
| `/items` | 264,843 (0.85 / 2.00; 24 MB) | 252,588 (0.73 / 4.53; 24 MB) | 18,645 | 1.05x faster |
| `/users/42` | 928,818 (0.23 / 0.91; 24 MB) | 505,317 (0.38 / 1.78; 25 MB) | 26,174 | 1.84x faster |
| `POST /orders` | 684,127 (0.33 / 1.01; 24 MB) | 310,087 (0.62 / 3.25; 24 MB) | 20,283 | 2.21x faster |

At 64 connections `/items` is 306,726 against Go's 269,187 (1.14x).

What changed:

- **`spawn`** went from 0.130 s to 0.041 s, level with Go. The main thread was spending its time in contended locks and in glibc's allocator. Now a fiber's record lives on its own stack, the global run queue is lock-free, and stacks move between threads in batches.
- **`/items`** went from 0.93x to 1.05x Go's rate. It was limited by how often the collector ran, over a thousand times a second, which the 4 MiB floor (`VIPER_GC_MIN_ALLOC`) cuts by 70%. Peak memory rose from 19 to 24 MB, as much as Go's.
- **`/users/42`** (a route with a parameter, behind ten others) went from 578,000 to 929,000 requests a second: the router no longer builds a dictionary for every route it tries.
- **`/plaintext`**, **`/json`** and **`POST /orders`** gained 18% to 63%, from the router, the response headers and the collector floor together.
- p99 latency fell on every endpoint.
- Programs with little live data use up to 4 MiB more before their first collection (`spawn`: 19 MB instead of 13 MB).

### 2026-11-19, performance plan phase 2

Phase 2 of [docs/09](../docs/09-performance-plan.md): inline allocation from per-thread free lists, pointer-free structs in memory the collector does not scan, and escape analysis for structs (docs/08, D83). Best of 3 runs.

| Benchmark | Viper | Go 1.27.4 | CPython 3.13.5 | Viper vs Go | Viper vs CPython | Memory: Viper / Go / CPython |
|---|---|---|---|---|---|---|
| binary_trees | 0.648 s | 0.831 s | 21.374 s | 1.28x faster | 33.0x faster | 90 / 40 / 56 MB |
| dispatch | 0.053 s | 0.031 s | 0.580 s | 1.72x the time | 10.9x faster | 13 / 13 / 13 MB |
| fib | 0.021 s | 0.037 s | 0.652 s | 1.80x faster | 31.8x faster | 13 / 13 / 13 MB |
| json_orders | 0.519 s | 0.637 s | 3.503 s | 1.23x faster | 6.7x faster | 81 / 84 / 130 MB |
| sieve | 0.052 s | 0.054 s | 2.243 s | 1.04x faster | 43.1x faster | 61 / 57 / 161 MB |
| sort | 0.539 s | 1.218 s | 2.184 s | 2.26x faster | 4.1x faster | 149 / 78 / 302 MB |
| spawn | 0.130 s | 0.042 s | 1.040 s | 3.13x the time | 8.0x faster | 13 / 14 / 47 MB |
| word_count | 0.720 s | 0.754 s | 2.390 s | 1.05x faster | 3.3x faster | 13 / 13 / 13 MB |

HTTP at 256 connections, requests per second (median / p99 latency in ms; peak memory), median of 3 runs of 5 s, server and load generator pinned to separate cores:

| Endpoint | Viper | Go 1.27.4 | Python 3.13.5 | Viper vs Go |
|---|---|---|---|---|
| `/plaintext` | 933,516 (0.23 / 0.67; 15 MB) | 566,764 (0.33 / 1.69; 24 MB) | 28,787 | 1.65x faster |
| `/json` | 882,838 (0.24 / 0.74; 15 MB) | 542,505 (0.35 / 1.74; 24 MB) | 27,508 | 1.63x faster |
| `/items` | 236,597 (1.07 / 2.02; 19 MB) | 254,760 (0.72 / 4.52; 24 MB) | 18,566 | 0.93x |
| `/users/42` | 578,410 (0.47 / 0.91; 15 MB) | 502,499 (0.37 / 1.90; 23 MB) | 25,136 | 1.15x faster |
| `POST /orders` | 418,592 (0.63 / 1.20; 15 MB) | 308,268 (0.59 / 3.76; 24 MB) | 19,843 | 1.36x faster |

At 64 connections `/items` is 253,940 against Go's 270,474 (0.94x). Python's server returned errors at 256 connections on every endpoint, as before.

What changed:

- **`binary_trees`** went from 0.93 s to 0.65 s and is now faster than Go. Every node comes from an inline pop off the thread's free list instead of a call into the collector. It peaks at 90 MB instead of 73 MB: the live data is the same, but one collection mid-run found more still reachable through stale stack words, and the heap kept that size (D83).
- **`json_orders`** is about 5% faster, with the same memory.
- **`/items`** went from 0.91x to 0.93x Go's rate, against a run in the same pinned setup just before the phase. That is within noise, so allocation is not what limits it. Phase 3 takes the collector's lock off the context switch next.
- The machine was slower for this run: Go's `json_orders` took 0.637 s against 0.580 s in phase 1, and the small-response HTTP rows moved by similar amounts for both servers. Compare ratios, not times.

### 2026-11-19, performance plan phase 1

Phase 1 of [docs/09](../docs/09-performance-plan.md): allocator and alias information for LLVM, inline `append` and string equality, builders on the stack (docs/08, D82). Best of 3 runs.

| Benchmark | Viper | Go 1.27.4 | CPython 3.13.5 | Viper vs Go | Viper vs CPython | Memory: Viper / Go / CPython |
|---|---|---|---|---|---|---|
| binary_trees | 0.933 s | 0.840 s | 19.410 s | 1.11x the time | 20.8x faster | 73 / 40 / 56 MB |
| dispatch | 0.049 s | 0.028 s | 0.564 s | 1.72x the time | 11.5x faster | 13 / 13 / 13 MB |
| fib | 0.022 s | 0.033 s | 0.601 s | 1.51x faster | 27.5x faster | 13 / 13 / 13 MB |
| json_orders | 0.498 s | 0.580 s | 3.246 s | 1.16x faster | 6.5x faster | 81 / 60 / 130 MB |
| sieve | 0.049 s | 0.057 s | 2.059 s | 1.16x faster | 41.8x faster | 59 / 54 / 161 MB |
| sort | 0.467 s | 1.106 s | 2.032 s | 2.37x faster | 4.3x faster | 147 / 81 / 302 MB |
| spawn | 0.148 s | 0.044 s | 0.956 s | 3.38x the time | 6.5x faster | 13 / 16 / 47 MB |
| word_count | 0.665 s | 0.747 s | 2.156 s | 1.12x faster | 3.2x faster | 13 / 13 / 13 MB |

HTTP at 256 connections, requests per second (median / p99 latency in ms), second of two runs:

| Endpoint | Viper | Go 1.27.4 | Python 3.13.5 | Viper vs Go |
|---|---|---|---|---|
| `/plaintext` | 928,678 (0.20 / 1.02) | 618,199 (0.27 / 1.91) | 21,944 | 1.50x faster |
| `/json` | 846,318 (0.21 / 1.12) | 583,292 (0.29 / 2.04) | 20,646 | 1.45x faster |
| `/items` | 228,440 (1.02 / 2.25) | 308,806 (0.38 / 5.34) | 14,172 | 0.74x |

What changed:

- **`sieve`** went from 1.20x Go's time to faster than Go (0.067 s to 0.049 s). The `append` fast path is inline, and alias information keeps the list's length and data pointer in registers through the marking loop. On its own, 20 million appends take 15 ms, against 33 ms before and Go's 18 ms.
- **`sort`** went from 0.71 s to 0.47 s (1.57x to 2.37x faster than Go). Building the list of keys is appends, and the comparison loop reads elements without reloading the list's fields.
- **Temporary structs** that do not escape after inlining are no longer allocated at all. No benchmark here builds them in a hot loop, but request handlers do.
- The `binary_trees` row is the same program and the same memory as before (73 MB, 0.93 s). The machine was slower for this run, as Go's 0.840 s against 0.769 s yesterday shows. The ratio, 1.11x, is unchanged within noise.
- **`/items`** moved only from 0.72x to 0.74x Go. Its allocations are mostly in the router, the response and the JSON body, which phases 2 and 3 address. The two HTTP runs differed by up to 15% on `/json` for both servers, so the small-response rows are noise-level changes.
- **`dispatch`** and **`spawn`** are untouched by phase 1; they are phase 4 and phase 3 work.

### 2026-11-18, milestone 12, first part

A quieter machine than for milestones 10 and 11 (Go's and CPython's times are back to the milestone 7 levels). Best of 3 runs.

| Benchmark | Viper | Go 1.27.4 | CPython 3.13.5 | Viper vs Go | Viper vs CPython | Memory: Viper / Go / CPython |
|---|---|---|---|---|---|---|
| binary_trees | 0.882 s | 0.795 s | 19.900 s | 1.11x the time | 22.6x faster | 72 / 40 / 56 MB |
| dispatch | 0.047 s | 0.027 s | 0.512 s | 1.76x the time | 10.9x faster | 13 / 13 / 13 MB |
| fib | 0.019 s | 0.031 s | 0.613 s | 1.68x faster | 33.1x faster | 13 / 13 / 13 MB |
| json_orders | 0.479 s | 0.540 s | 3.240 s | 1.13x faster | 6.8x faster | 81 / 58 / 130 MB |
| sieve | 0.072 s | 0.058 s | 1.801 s | 1.25x the time | 25.0x faster | 60 / 65 / 161 MB |
| sort | 0.696 s | 1.110 s | 2.071 s | 1.59x faster | 3.0x faster | 186 / 81 / 302 MB |
| spawn | 0.146 s | 0.045 s | 0.926 s | 3.26x the time | 6.4x faster | 13 / 14 / 47 MB |
| word_count | 0.605 s | 0.698 s | 2.027 s | 1.15x faster | 3.4x faster | 13 / 13 / 13 MB |

HTTP at 256 connections, requests per second (median / p99 latency in ms):

| Endpoint | Viper | Go 1.27.4 | Python 3.13.5 | Viper vs Go |
|---|---|---|---|---|
| `/plaintext` | 815,528 (0.24 / 1.16) | 545,193 (0.30 / 2.21) | 15,765 | 1.50x faster |
| `/json` | 767,318 (0.26 / 1.18) | 519,676 (0.31 / 2.28) | 16,647 | 1.48x faster |
| `/items` | 218,610 (1.11 / 2.54) | 303,226 (0.39 / 5.51) | 11,007 | 0.72x |

What changed (docs/08, D73 to D75):

- **`word_count`**: `counts[key] = counts.get_or(key, 0) + 1` now compiles to one dict lookup instead of two: 1.25 s to 0.61 s, from 1.47x Go's time to faster than Go.
- **The collector's growth policy**: the heap may grow to about twice the live data before a collection, like Go's default, instead of Boehm's default of a third. Allocation-heavy programs got 3% to 7% faster and use 10% to 25% more memory (`binary_trees`, 57 MB to 72 MB); the HTTP server gained much more, because it collected very often with a small live heap.
- **Responses** are serialised once, straight from the handler's strings.
- Together these took `/items` from 114,000 to 219,000 requests per second (0.42x to 0.72x Go) and the small-response endpoints from 1.2x to 1.5x Go's rate. Go's median latency on `/items` is still a third of Viper's: the remaining cost is allocation, which is what escape analysis is for.
- **`spawn`** is unchanged for Viper (0.146 s here, on the quieter machine); Go's time dropped further, so the ratio moved from 2.7x to 3.3x.

### 2026-11-18, end of milestone 11: frame pointers and debug information

Generated code now keeps frame pointers and carries DWARF line tables (docs/08, D67). Best of 3 runs, the machine as busy as for milestone 10:

| Benchmark | Viper | Go 1.27.4 | CPython 3.13.5 | Viper vs Go | Viper vs CPython |
|---|---|---|---|---|---|
| binary_trees | 1.150 s | 0.936 s | 23.303 s | 1.23x the time | 20.3x faster |
| dispatch | 0.054 s | 0.035 s | 0.600 s | 1.55x the time | 11.1x faster |
| fib | 0.022 s | 0.036 s | 0.720 s | 1.68x faster | 33.4x faster |
| json_orders | 0.614 s | 0.708 s | 3.581 s | 1.15x faster | 5.8x faster |
| sieve | 0.088 s | 0.067 s | 2.225 s | 1.31x the time | 25.2x faster |
| sort | 0.829 s | 1.249 s | 2.224 s | 1.51x faster | 2.7x faster |
| spawn | 0.172 s | 0.056 s | 1.393 s | 3.09x the time | 8.1x faster |
| word_count | 1.244 s | 0.845 s | 2.402 s | 1.47x the time | 1.9x faster |

Only `fib`, which is nothing but calls, moved: 0.020 s to 0.022 s, the cost of the frame pointer in each call. `spawn` is unchanged for Viper; Go's time varied between runs (0.056 to 0.065 s).

### 2026-11-18, end of milestone 10: fibers

Tasks and connections now run on fibers: one worker thread per core (12 here), an epoll poller, and parking instead of blocked threads.

The machine was busier during these runs than during the earlier ones (Go and CPython are also 10 to 20% slower than in the milestone 7 table), so compare the ratios, not the times. Best of 3 runs.

| Benchmark | Viper | Go 1.27.4 | CPython 3.13.5 | Viper vs Go | Viper vs CPython | Memory: Viper / Go / CPython |
|---|---|---|---|---|---|---|
| binary_trees | 1.181 s | 0.917 s | 23.292 s | 1.29x the time | 19.7x faster | 57 / 40 / 56 MB |
| dispatch | 0.053 s | 0.032 s | 0.598 s | 1.66x the time | 11.4x faster | 13 / 13 / 13 MB |
| fib | 0.020 s | 0.038 s | 0.711 s | 1.95x faster | 36.0x faster | 13 / 13 / 13 MB |
| json_orders | 0.620 s | 0.684 s | 3.565 s | 1.10x faster | 5.8x faster | 67 / 62 / 130 MB |
| sieve | 0.086 s | 0.068 s | 2.165 s | 1.26x the time | 25.1x faster | 51 / 54 / 161 MB |
| sort | 0.817 s | 1.242 s | 2.228 s | 1.52x faster | 2.7x faster | 172 / 79 / 302 MB |
| spawn | 0.175 s | 0.065 s | 1.520 s | 2.69x the time | 8.7x faster | |
| word_count | 1.277 s | 0.828 s | 2.384 s | 1.54x the time | 1.9x faster | 13 / 13 / 13 MB |

Generated code now has unwind tables instead of being marked `nounwind`, so that a panic can end just its task (docs/08, D64). The single-threaded benchmarks show no change in their ratios to Go beyond noise.

HTTP, requests per second, with median and 99th-percentile latency in milliseconds:

| Connections | Endpoint | Viper | Go 1.27.4 | Python 3.13.5 | Viper vs Go |
|---|---|---|---|---|---|
| 64 | `/plaintext` | 687,328 (0.06 / 0.43) | 548,816 (0.07 / 0.68) | 20,717 | 1.25x faster |
| 64 | `/json` | 644,187 (0.06 / 0.46) | 513,588 (0.08 / 0.70) | 17,681 | 1.25x faster |
| 64 | `/items` | 140,477 (0.44 / 1.01) | 250,686 (0.15 / 1.31) | 13,114 | 0.56x |
| 256 | `/plaintext` | 687,956 (0.33 / 1.06) | 554,712 (0.30 / 2.17) | 18,054 | 1.24x faster |
| 256 | `/json` | 606,247 (0.39 / 1.15) | 536,999 (0.31 / 2.27) | 16,176 | 1.13x faster |
| 256 | `/items` | 114,476 (2.03 / 5.21) | 271,004 (0.42 / 6.33) | 11,538 | 0.42x |

Idle WebSockets: 50,000 connections opened in 3.1 s, all still echoing afterwards; the server grew from 3.7 MB to 569 MB, **11.3 KiB per connection**. With a thread per connection (milestone 9) each connection also held a thread with its own stack and kernel scheduling state.

Notes:

- **Small responses** stay ahead of Go, and Viper's 99th-percentile latency is lower than Go's on every endpoint at both connection counts.
- **`/items`** (20 records and 1.1 KB of JSON per request, about a hundred allocations) is where the collector shows, as at Gate 2. It improved slightly at 64 connections (0.51x to 0.56x), since 12 workers contend for Boehm's allocator less than 64 threads did, but at 256 connections Viper's median latency is five times Go's. This is Gate 3's open item; see docs/06.
- **`spawn`** costs about 0.9 µs per task against Go's 0.3 µs: a stack from the pool, the fiber record, registering it with the collector under its lock, and a queue. Keeping up to 16,384 stacks for reuse (instead of 1,024) took it from 0.22 s to 0.175 s, since most stacks then skip `mmap` and fresh page faults.

### 2026-11-18, end of milestone 8: HTTP

64 connections, 5 seconds per run, requests per second. Viper's server is stage 1: one OS thread per connection.

| Endpoint | Viper | Go 1.27.4 | Python 3.13.5 | Viper vs Go | Viper vs Python |
|---|---|---|---|---|---|
| `/plaintext` | 733,972 | 565,280 | 24,476 | 1.30x faster | 30.0x faster |
| `/json` | 716,590 | 530,883 | 21,966 | 1.35x faster | 32.6x faster |
| `/items` | 134,075 | 262,913 | 16,502 | 0.51x (Go is 1.96x faster) | 8.1x faster |

Notes:

- **Small responses are faster than Go.** With 64 connections on 12 hardware threads, a thread per connection costs little, and the request path allocates almost nothing: the request is parsed on the connection thread's stack, and only the method, path and body become Viper strings.
- **`/items` is where the collector shows.** It allocates 20 records, their names and tag lists, and a 1.1 KB string per request: about a hundred allocations. Running it with a large initial heap (`VIPER_GC_INITIAL_HEAP=1000000000`) or `GC_FREE_SPACE_DIVISOR=1` raises it to about 195,000 requests per second (+50%), so about a third of the time is collection. Most of the rest is contention on Boehm's allocator across 64 threads.
- **Gate 2** (docs/06, "Gates"): on the JSON endpoints Viper is ahead of Go for small responses and at half Go's speed for large ones. Collection is about a third of the `/items` time; the larger part is 64 threads contending for the allocator, which is the thread model. The gate's answer is therefore to proceed to stage 2 (fibers on one worker per core) and measure again at Gate 3; see docs/06.

### 2026-11-18, end of milestone 7

Same machine and versions. Best of 3 runs.

| Benchmark | Viper | Go 1.27.4 | CPython 3.13.5 | Viper vs Go | Viper vs CPython | Memory: Viper / Go / CPython |
|---|---|---|---|---|---|---|
| binary_trees | 0.968 s | 0.789 s | 20.137 s | 1.23x the time | 20.8x faster | 57 / 38 / 56 MB |
| dispatch | 0.045 s | 0.026 s | 0.503 s | 1.68x the time | 11.3x faster | 13 / 13 / 13 MB |
| fib | 0.019 s | 0.031 s | 0.609 s | 1.61x faster | 31.9x faster | 13 / 13 / 13 MB |
| json_orders | 0.524 s | 0.543 s | 3.231 s | 1.04x faster | 6.2x faster | 75 / 62 / 129 MB |
| sieve | 0.067 s | 0.054 s | 1.947 s | 1.23x the time | 29.1x faster | 52 / 72 / 161 MB |
| sort | 0.748 s | 1.124 s | 2.063 s | 1.50x faster | 2.8x faster | 169 / 75 / 303 MB |
| word_count | 1.059 s | 0.739 s | 2.018 s | 1.43x the time | 1.9x faster | 13 / 13 / 13 MB |

Notes:

- **Typed JSON** meets the docs/07 target of "within 1.5x of Go's `encoding/json`": it is slightly faster. The encoder and decoder are generated per type, so there is no reflection and no intermediate tree; Go's default encoder uses reflection. Viper writes compact JSON, as Go does.
- **A regression found and fixed.** Midway through milestone 7, `word_count` measured 1.51 s instead of 1.06 s. The cause was milestone 6's call to `GC_allow_register_threads` at startup, which puts Boehm into its multi-threaded mode and makes every allocation slower, even in programs that never start a task. It is now called on the first `spawn` instead (D46), and single-threaded programs are back to their earlier speed. A program that spawns pays the cost from then on.

### 2026-11-18, end of milestone 4

Same machine and versions as below. Best of 3 runs.

| Benchmark | Viper | Go 1.27.4 | CPython 3.13.5 | Viper vs Go | Viper vs CPython | Memory: Viper / Go / CPython |
|---|---|---|---|---|---|---|
| binary_trees | 0.963 s | 0.799 s | 20.125 s | 1.20x the time | 20.9x faster | 65 / 42 / 56 MB |
| dispatch | 0.046 s | 0.027 s | 0.539 s | 1.70x the time | 11.8x faster | 13 / 13 / 13 MB |
| fib | 0.018 s | 0.030 s | 0.616 s | 1.68x faster | 34.0x faster | 13 / 13 / 13 MB |
| sieve | 0.067 s | 0.055 s | 1.857 s | 1.22x the time | 27.5x faster | 55 / 54 / 161 MB |
| sort | 0.744 s | 1.115 s | 2.046 s | 1.50x faster | 2.8x faster | 169 / 77 / 302 MB |
| word_count | 1.064 s | 0.716 s | 2.056 s | 1.49x the time | 1.9x faster | 13 / 13 / 13 MB |

Notes on `sort`:

- Viper computes each key once (as Python does) and then sorts with a comparison of the keys, so the closure runs a million times rather than once per comparison. Go's `sort.SliceStable` calls its comparator closure about 20 million times and swaps elements through reflection, which is where it loses.
- CPython's sort is written in C, so its margin here is small: most of its time is building the records and calling the key lambda.
- Viper's memory is higher than Go's because the keys of the tuple sort are kept in a separate list of `(str, int)` tuples, 24 bytes each.

### 2026-11-18, end of milestone 3

AMD Ryzen AI 5 340 (12 threads), Debian 13, LLVM 19.1.7, Boehm GC 8.2.8, Go 1.27.4, CPython 3.13.5. Best of 5 runs, wall-clock time; memory is peak resident set size.

| Benchmark | Viper | Go 1.27.4 | CPython 3.13.5 | Viper vs Go | Viper vs CPython | Memory: Viper / Go / CPython |
|---|---|---|---|---|---|---|
| binary_trees | 0.957 s | 0.781 s | 20.309 s | 1.22x the time | 21.2x faster | 65 / 39 / 56 MB |
| dispatch | 0.046 s | 0.026 s | 0.505 s | 1.74x the time | 11.1x faster | 13 / 13 / 13 MB |
| fib | 0.017 s | 0.030 s | 0.607 s | 1.76x faster | 35.3x faster | 13 / 13 / 13 MB |
| sieve | 0.067 s | 0.053 s | 1.834 s | 1.28x the time | 27.2x faster | 54 / 62 / 161 MB |
| word_count | 1.077 s | 0.685 s | 2.007 s | 1.57x the time | 1.9x faster | 13 / 13 / 13 MB |

Milestone 3 did not change the four earlier benchmarks beyond run-to-run noise.

**Against the targets in docs/07** ("Targets", end-of-prototype column), every target is already met:

| Group | Target vs Go | Now |
|---|---|---|
| CPU (`fib`) | within 1.3x | faster than Go |
| Allocation (`binary_trees`) | within 3x | 1.22x, inside even the post-tuning target of 1.5x |
| Collections and strings (`sieve`, `word_count`) | within 2x | 1.28x and 1.57x |

docs/07 has no numeric target for dispatch; it is measured to compare trait objects with the generics that arrive in milestone 4.

**Gate 1** (docs/06-roadmap.md, "Gates") asks whether allocation-heavy code is at least 10x faster than CPython and whether the GC stress mode is clean. Both hold: `binary_trees` is 21x faster, and every test program passes with a collection before every allocation.

Notes:

- `fib` flatters the compiler: LLVM turns part of the recursion into a loop, as it would for C or Rust. Go's compiler does not.
- `binary_trees` uses more memory than Go (65 MB against 38 MB). Each object carries an 8-byte descriptor header, and the conservative collector grows the heap more eagerly than Go's.
- `dispatch` costs about 4.6 ns per call against Go's 2.6 ns. Both load a function pointer from a table and call it; the gap is unexplained so far and worth a profile once debug info exists. Generics (milestone 4) will give a static-dispatch alternative.
- `sieve` trails Go because every `append` is a call into the runtime, whereas Go inlines the common case. The optimisation backlog in docs/07 covers this: collections rewritten in the language, so the fast path inlines.
- `word_count` is the weakest against CPython. A string-keyed dict lookup costs about 20 ns, against 4 ns for an int key. The idiom `counts[k] = counts.get_or(k, 0) + 1` does two lookups, where Go does one, and `split()` builds a new list each time. CPython caches each string's hash, which offsets its interpreter overhead on this workload. The planned fixes are on the same backlog: inlinable collections, and an entry API that updates a value with one lookup.
