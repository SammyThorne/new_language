# Web performance: what to expect

A rough guide to what a Weby web service can do, measured against the same service written in Go (`net/http`) and in Python (the standard library's HTTP server). The figures are from one laptop and a loopback network. Treat them as orders of magnitude and ratios, not as promises for production hardware. Measured on 2026-11-19, after phases 1 to 4 of the [performance plan](09-performance-plan.md).

## In short

| Question | Rough answer |
|---|---|
| How much CPU does the server itself cost per request? | 2.5 to 5 µs for a small JSON request or response; about 10 µs to build and encode a 1 KB JSON list. About half of Go's cost, and 1/30th of Python's |
| How many requests per second per core? | 300,000 to 380,000 small requests; about 100,000 for a handler that builds and encodes a 20-record list |
| On a 3-core (6-thread) slice of a laptop? | About 1.1 million small requests per second, or 280,000 of the 20-record list |
| Added latency at 256 concurrent connections | Median 0.2 ms, 99th percentile 0.7 ms for small responses; 0.8 / 1.8 ms for the 20-record list. Go's 99th percentile is 2 to 3.5 times higher |
| Memory | 4 MB idle; 15 to 35 MB under full load; about 15 KiB per open, idle WebSocket |
| Start-up | 4 ms to the first response; one 7.6 MB executable that needs only `libgc` |

**What this means for a real service:** the framework is unlikely to be the bottleneck. At 3 to 10 µs of server CPU per request, a typical handler that waits 1 to 5 ms on a database spends well under 1% of its time in Weby's HTTP layer. Throughput is then decided by the database, the network and the handler's own work. Where the handler's work is CPU-bound (building and encoding JSON, string processing), Weby runs it at or ahead of Go's speed (see "The language itself" below).

## The setup

| | |
|---|---|
| Machine | Laptop with an AMD Ryzen AI 5 340 (6 cores, 12 threads), 15 GB, Linux 6.12 |
| Server | Pinned to 6 logical CPUs (3 cores with SMT), or to 1 for the per-core figures |
| Load | `bench/http/load.go`, a closed-loop generator: N keep-alive connections, each sending its next request as soon as the last response arrives. It runs on the other 6 logical CPUs, over loopback |
| Compared with | Go 1.27.4 with `net/http` and `encoding/json`; Python 3.13.5 with `http.server.ThreadingHTTPServer` and `json` (no framework is installed; FastAPI or similar would differ) |
| Runs | 5 s each, median of 3 |

The service (`bench/http/server.*`, the same in all three languages) has five endpoints:

| Endpoint | What the handler does |
|---|---|
| `/plaintext` | Returns `Hello, World!` |
| `/json` | Encodes a one-field struct: `{"message":"Hello, World!"}` |
| `/items` | Builds 20 records, each with a formatted name, a float and a list of tags, and encodes them: 1.1 KB of JSON |
| `/users/{id}` | A typed route with a path parameter, registered behind ten other routes; returns a JSON record |
| `POST /orders` | Decodes a JSON order with five line items into structs, totals it, and returns a JSON receipt |

## Throughput and latency

Requests per second, with median and 99th-percentile latency in milliseconds and the server's peak memory.

**64 connections**

| Endpoint | Weby | Go | Python | Weby vs Go |
|---|---|---|---|---|
| `/plaintext` | 1,067,709 (0.05 / 0.20; 16 MB) | 591,167 (0.08 / 0.47; 19 MB) | 30,792 (1.31 / 5.61) | 1.81x |
| `/json` | 1,112,032 (0.05 / 0.18; 16 MB) | 579,947 (0.08 / 0.46; 18 MB) | 28,937 (1.45 / 6.05) | 1.92x |
| `/items` | 304,563 (0.19 / 0.62; 19 MB) | 269,204 (0.17 / 1.03; 18 MB) | 20,369 (1.14 / 12.65) | 1.13x |
| `/users/42` | 986,005 (0.06 / 0.34; 21 MB) | 535,306 (0.09 / 0.50; 20 MB) | 26,914 (1.33 / 6.35) | 1.84x |
| `POST /orders` | 697,896 (0.09 / 0.33; 14 MB) | 320,510 (0.14 / 0.87; 19 MB) | 21,107 (1.47 / 9.88) | 2.18x |

**256 connections**

| Endpoint | Weby | Go | Python | Weby vs Go |
|---|---|---|---|---|
| `/plaintext` | 1,126,626 (0.19 / 0.68; 25 MB) | 564,646 (0.34 / 1.62; 23 MB) | 30,500 (4.25 / 17.16) | 2.00x |
| `/json` | 1,110,401 (0.19 / 0.69; 24 MB) | 542,519 (0.35 / 1.68; 24 MB) | 27,857 (3.96 / 16.82) | 2.05x |
| `/items` | 278,843 (0.82 / 1.80; 33 MB) | 258,908 (0.73 / 4.03; 23 MB) | 19,338 (2.98 / 23.31) | 1.08x |
| `/users/42` | 971,131 (0.22 / 0.86; 33 MB) | 504,215 (0.38 / 1.79; 24 MB) | 25,817 (2.57 / 10.74) | 1.93x |
| `POST /orders` | 690,164 (0.33 / 1.00; 30 MB) | 306,181 (0.62 / 3.49; 24 MB) | 20,528 (3.42 / 25.97) | 2.25x |

The Python server dropped a few hundred connections at 256 on every endpoint; its rows count only successful requests.

**What to read from this:**
- **Small requests.** On requests where the server does little, Weby serves about twice Go's rate, with lower latency at both percentiles. The difference is the cost of the HTTP layer itself: parsing, routing, writing the response, and switching between connections.
- **`/items`.** Here the handler's own work dominates: about a hundred small allocations and 1.1 KB of JSON per request. The two are close, Weby 8% to 13% ahead. Go's median latency is slightly lower, but its 99th percentile is more than twice Weby's.
- **Tails.** Weby's 99th-percentile latency is the lowest of the three on every row.
- **Python** is 15 to 40 times slower than either.

## Per core

Each server on a single logical CPU (`WEBY_WORKERS=1`, `GOMAXPROCS=1`), 64 connections, best of 3 runs (`bench/http/one_core.py`). The CPU time per request is one second divided by the rate.

| Endpoint | Weby | Go | Weby vs Go | CPU per request: Weby / Go |
|---|---|---|---|---|
| `/plaintext` | 382,310 | 186,777 | 2.05x | 2.6 / 5.4 µs |
| `/json` | 377,275 | 179,428 | 2.10x | 2.7 / 5.6 µs |
| `/items` | 104,364 | 91,637 | 1.14x | 9.6 / 10.9 µs |
| `/users/42` | 315,783 | 170,838 | 1.85x | 3.2 / 5.9 µs |
| `POST /orders` | 219,666 | 110,386 | 1.99x | 4.6 / 9.1 µs |

These rates include the kernel's share of loopback networking, which a real network would replace with its own costs.

**For sizing:** take the server CPU per request from this table, add the handler's own work, and divide into the cores available. Leave headroom: these runs keep every core fully busy, which a production service should not.

Six logical CPUs gave about three times the one-CPU rate, not six. Those six are three physical cores with two threads each, and the load generator was busy on the other six. Expect closer to linear scaling across physical cores.

## Memory

| | Weby | Go | Python |
|---|---|---|---|
| Idle server | 4.2 MB | 10.9 MB | 18.2 MB |
| Peak under load (256 connections) | 24 to 33 MB | 23 to 24 MB | 24 to 27 MB |
| Executable | 7.6 MB, needs only `libgc` at run time | 9.8 MB, static | the interpreter |
| Start-up to first response | 4 ms | 4 ms | 29 ms |

- **Under load.** Weby's peak is set mostly by the collector: it allocates at least 4 MiB between collections (`WEBY_GC_MIN_ALLOC`, the same minimum as Go's heap), so a busy server sits a few MB above its live data. Setting the variable lower trades memory for collection time.
- **Idle WebSockets.** `bench/ws_idle/` opened 50,000 WebSocket connections in 2.2 s and held them idle, and all 50,000 still echoed afterwards. The server grew from 4.2 MB to 763 MB: **15.5 KiB per connection**, almost all of it the touched pages of each connection's task stack.
  - **Rough capacity:** 1 GB of memory holds about 65,000 idle connections.
  - **Compared with earlier:** the same measurement gave 11.3 KiB at milestone 10, and 16.5 KiB at the end of the performance plan's phase 2. The cause of that growth has not been looked into.

## The language itself

Handlers do more than the HTTP layer. These are the standard benchmarks in `bench/` (best of 3 runs, [bench/README.md](../bench/README.md)), with the ones most relevant to web services first:

| Benchmark | What it exercises | Weby | Go | Python | Weby vs Go |
|---|---|---|---|---|---|
| `json_orders` | Typed JSON encode and decode of 20,000 orders, ten times | 0.523 s | 0.602 s | 3.390 s | 1.15x faster |
| `word_count` | Splitting, lower-casing, string-keyed dicts | 0.648 s | 0.778 s | 2.331 s | 1.20x faster |
| `sort` | Sorting a million records with a key function | 0.477 s | 1.132 s | 2.094 s | 2.37x faster |
| `spawn` | Starting and joining 200,000 tasks | 0.040 s | 0.042 s | 0.993 s | 1.03x faster |
| `dispatch` | 10 million calls through trait objects | 0.009 s | 0.031 s | 0.591 s | 3.38x faster |
| `binary_trees` | Allocation and collection | 0.646 s | 0.834 s | 20.149 s | 1.29x faster |
| `fib` | Function calls | 0.021 s | 0.036 s | 0.644 s | 1.73x faster |
| `sieve` | List indexing and appends | 0.051 s | 0.052 s | 2.277 s | 1.03x faster |

Weby is at or ahead of Go on all eight, and 3.6 to 64 times faster than CPython. The weak spot is memory in allocation-heavy programs: `binary_trees` peaks at 90 MB against Go's 42 MB, and `sort` at 148 MB against 79 MB. That is the conservative collector's cost, which phase 5 of the plan is about.

## What these numbers do not cover

- **One machine, loopback, client and server together.** A real network adds latency and its own CPU costs, and production servers have more cores and different caches.
- **No TLS.** The servers support HTTPS (`rustls` for Weby), but these runs are plain HTTP. TLS adds a per-connection handshake and per-byte encryption on both sides alike.
- **No database.** The PostgreSQL driver is not part of these benchmarks. With a database, its latency usually decides a request's time.
- **Python's baseline.** Python is the standard library's threaded server, the weakest option there is; an ASGI framework under uvicorn would do several times better, though still far from Go or Weby.
- **Closed-loop load.** Each connection waits for its response before sending the next request, which understates tail latency under overload compared with an open-loop generator at a fixed arrival rate.
- **HTTP/1.1 only**, with keep-alive. Weby does not speak HTTP/2.

## Reproducing

```bash
make release
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

```bash
python3 bench/run.py --runs 3
```

Go must be installed for the comparisons (`~/.local/go/bin` is searched too). `bench/profile/` has a small sampling profiler for finding where a handler's time goes on machines without `perf`.
