# Benchmarks

Each benchmark exists as a Weby program (`.wb`), a Go program (`.go`) and a CPython program (`.py`), all printing the same output. `run.py` builds the Weby version with `--release` and the Go version with `go build`, runs all three, checks that the outputs match, and prints a table. Go is found on `PATH` or in `~/.local/go/bin`; without it, the Go column is left out.

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

`bench/http/` holds the same small service three times: `server.wb`, `server.go` (`net/http`) and `server.py` (the standard library's `ThreadingHTTPServer`, since no Python web framework is installed and installing one is a download). Each answers three endpoints:

| Endpoint | What it does |
|---|---|
| `/plaintext` | Returns a fixed string |
| `/json` | Encodes a small struct to JSON |
| `/items` | Builds and encodes a list of 100 structs, about 6 KB of JSON |

`load.go` is a closed-loop load generator: N connections, each sending the next request as soon as the previous response arrives, over keep-alive. It runs on the same machine as the server.

```bash
python3 bench/http/run.py --seconds 5 --connections 64
```

## Idle WebSockets and task creation

`ws_idle/` holds a WebSocket echo server (`server.wb`) and a Go client (`client.go`) that opens N connections from several loopback addresses, holds them idle, then checks that each still echoes. `run.py` measures the server's resident memory before and after.

```bash
python3 bench/ws_idle/run.py --connections 50000
```

`spawn` (in the main suite) starts 10,000 tasks that each add two numbers, joins them, twenty times. Go uses goroutines with a result channel each; Python a pool of 64 threads.

## Results

### 2026-11-18, end of milestone 10: fibers

Tasks and connections now run on fibers: one worker thread per core (12 here), an epoll poller, and parking instead of blocked threads.

The machine was busier during these runs than during the earlier ones (Go and CPython are also 10 to 20% slower than in the milestone 7 table), so compare the ratios, not the times. Best of 3 runs.

| Benchmark | Weby | Go 1.27.4 | CPython 3.13.5 | Weby vs Go | Weby vs CPython | Memory: Weby / Go / CPython |
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

| Connections | Endpoint | Weby | Go 1.27.4 | Python 3.13.5 | Weby vs Go |
|---|---|---|---|---|---|
| 64 | `/plaintext` | 687,328 (0.06 / 0.43) | 548,816 (0.07 / 0.68) | 20,717 | 1.25x faster |
| 64 | `/json` | 644,187 (0.06 / 0.46) | 513,588 (0.08 / 0.70) | 17,681 | 1.25x faster |
| 64 | `/items` | 140,477 (0.44 / 1.01) | 250,686 (0.15 / 1.31) | 13,114 | 0.56x |
| 256 | `/plaintext` | 687,956 (0.33 / 1.06) | 554,712 (0.30 / 2.17) | 18,054 | 1.24x faster |
| 256 | `/json` | 606,247 (0.39 / 1.15) | 536,999 (0.31 / 2.27) | 16,176 | 1.13x faster |
| 256 | `/items` | 114,476 (2.03 / 5.21) | 271,004 (0.42 / 6.33) | 11,538 | 0.42x |

Idle WebSockets: 50,000 connections opened in 3.1 s, all still echoing afterwards; the server grew from 3.7 MB to 569 MB, **11.3 KiB per connection**. With a thread per connection (milestone 9) each connection also held a thread with its own stack and kernel scheduling state.

Notes:

- **Small responses** stay ahead of Go, and Weby's 99th-percentile latency is lower than Go's on every endpoint at both connection counts.
- **`/items`** (100 structs and 6 KB of JSON per request) is where the collector shows, as at Gate 2. It improved slightly at 64 connections (0.51x to 0.56x), since 12 workers contend for Boehm's allocator less than 64 threads did, but at 256 connections Weby's median latency is five times Go's. This is Gate 3's open item; see docs/06.
- **`spawn`** costs about 0.9 µs per task against Go's 0.3 µs: a stack from the pool, the fiber record, registering it with the collector under its lock, and a queue. Keeping up to 16,384 stacks for reuse (instead of 1,024) took it from 0.22 s to 0.175 s, since most stacks then skip `mmap` and fresh page faults.

### 2026-11-18, end of milestone 8: HTTP

64 connections, 5 seconds per run, requests per second. Weby's server is stage 1: one OS thread per connection.

| Endpoint | Weby | Go 1.27.4 | Python 3.13.5 | Weby vs Go | Weby vs Python |
|---|---|---|---|---|---|
| `/plaintext` | 733,972 | 565,280 | 24,476 | 1.30x faster | 30.0x faster |
| `/json` | 716,590 | 530,883 | 21,966 | 1.35x faster | 32.6x faster |
| `/items` | 134,075 | 262,913 | 16,502 | 0.51x (Go is 1.96x faster) | 8.1x faster |

Notes:

- **Small responses are faster than Go.** With 64 connections on 12 hardware threads, a thread per connection costs little, and the request path allocates almost nothing: the request is parsed on the connection thread's stack, and only the method, path and body become Weby strings.
- **`/items` is where the collector shows.** It allocates about 100 structs and a 6 KB string per request. Running it with a large initial heap (`WEBY_GC_INITIAL_HEAP=1000000000`) or `GC_FREE_SPACE_DIVISOR=1` raises it to about 195,000 requests per second (+50%), so about a third of the time is collection. Most of the rest is contention on Boehm's allocator across 64 threads.
- **Gate 2** (docs/06, "Gates"): on the JSON endpoints Weby is ahead of Go for small responses and at half Go's speed for large ones. Collection is about a third of the `/items` time; the larger part is 64 threads contending for the allocator, which is the thread model. The gate's answer is therefore to proceed to stage 2 (fibers on one worker per core) and measure again at Gate 3; see docs/06.

### 2026-11-18, end of milestone 7

Same machine and versions. Best of 3 runs.

| Benchmark | Weby | Go 1.27.4 | CPython 3.13.5 | Weby vs Go | Weby vs CPython | Memory: Weby / Go / CPython |
|---|---|---|---|---|---|---|
| binary_trees | 0.968 s | 0.789 s | 20.137 s | 1.23x the time | 20.8x faster | 57 / 38 / 56 MB |
| dispatch | 0.045 s | 0.026 s | 0.503 s | 1.68x the time | 11.3x faster | 13 / 13 / 13 MB |
| fib | 0.019 s | 0.031 s | 0.609 s | 1.61x faster | 31.9x faster | 13 / 13 / 13 MB |
| json_orders | 0.524 s | 0.543 s | 3.231 s | 1.04x faster | 6.2x faster | 75 / 62 / 129 MB |
| sieve | 0.067 s | 0.054 s | 1.947 s | 1.23x the time | 29.1x faster | 52 / 72 / 161 MB |
| sort | 0.748 s | 1.124 s | 2.063 s | 1.50x faster | 2.8x faster | 169 / 75 / 303 MB |
| word_count | 1.059 s | 0.739 s | 2.018 s | 1.43x the time | 1.9x faster | 13 / 13 / 13 MB |

Notes:

- **Typed JSON** meets the docs/07 target of "within 1.5x of Go's `encoding/json`": it is slightly faster. The encoder and decoder are generated per type, so there is no reflection and no intermediate tree; Go's default encoder uses reflection. Weby writes compact JSON, as Go does.
- **A regression found and fixed.** Midway through milestone 7, `word_count` measured 1.51 s instead of 1.06 s. The cause was milestone 6's call to `GC_allow_register_threads` at startup, which puts Boehm into its multi-threaded mode and makes every allocation slower, even in programs that never start a task. It is now called on the first `spawn` instead (D46), and single-threaded programs are back to their earlier speed. A program that spawns pays the cost from then on.

### 2026-11-18, end of milestone 4

Same machine and versions as below. Best of 3 runs.

| Benchmark | Weby | Go 1.27.4 | CPython 3.13.5 | Weby vs Go | Weby vs CPython | Memory: Weby / Go / CPython |
|---|---|---|---|---|---|---|
| binary_trees | 0.963 s | 0.799 s | 20.125 s | 1.20x the time | 20.9x faster | 65 / 42 / 56 MB |
| dispatch | 0.046 s | 0.027 s | 0.539 s | 1.70x the time | 11.8x faster | 13 / 13 / 13 MB |
| fib | 0.018 s | 0.030 s | 0.616 s | 1.68x faster | 34.0x faster | 13 / 13 / 13 MB |
| sieve | 0.067 s | 0.055 s | 1.857 s | 1.22x the time | 27.5x faster | 55 / 54 / 161 MB |
| sort | 0.744 s | 1.115 s | 2.046 s | 1.50x faster | 2.8x faster | 169 / 77 / 302 MB |
| word_count | 1.064 s | 0.716 s | 2.056 s | 1.49x the time | 1.9x faster | 13 / 13 / 13 MB |

Notes on `sort`:

- Weby computes each key once (as Python does) and then sorts with a comparison of the keys, so the closure runs a million times rather than once per comparison. Go's `sort.SliceStable` calls its comparator closure about 20 million times and swaps elements through reflection, which is where it loses.
- CPython's sort is written in C, so its margin here is small: most of its time is building the records and calling the key lambda.
- Weby's memory is higher than Go's because the keys of the tuple sort are kept in a separate list of `(str, int)` tuples, 24 bytes each.

### 2026-11-18, end of milestone 3

AMD Ryzen AI 5 340 (12 threads), Debian 13, LLVM 19.1.7, Boehm GC 8.2.8, Go 1.27.4, CPython 3.13.5. Best of 5 runs, wall-clock time; memory is peak resident set size.

| Benchmark | Weby | Go 1.27.4 | CPython 3.13.5 | Weby vs Go | Weby vs CPython | Memory: Weby / Go / CPython |
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
