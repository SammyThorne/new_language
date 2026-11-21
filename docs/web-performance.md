# Web performance: what to expect

A rough guide to what a Viper web service can do, measured against the same service written in Go (`net/http`) and in Python (the standard library's HTTP server). The figures are from one laptop and a loopback network. Treat them as orders of magnitude and ratios, not as promises for production hardware. Measured on 2026-11-19, after the [performance plan](09-performance-plan.md) and the [final optimisation plan](10-final-optimization-plan.md).

## In short

| Question | Rough answer |
|---|---|
| How much CPU does the server itself cost per request? | About 2 µs for a small request or response; about 3.6 µs to build and encode a 1 KB JSON list. 35% to 40% of Go's cost, and 1/40th of Python's |
| How many requests per second per core? | 430,000 to 530,000 small requests; about 280,000 for a handler that builds and encodes a 20-record list |
| On a 3-core (6-thread) slice of a laptop? | About 960,000 small requests per second (the load generator's limit on this machine, not the server's), or 530,000 of the 20-record list |
| Added latency at 256 concurrent connections | Median 0.2 ms, 99th percentile 0.9 ms for small responses; 0.4 / 1.1 ms for the 20-record list. Go's 99th percentile is 2 to 5 times higher |
| Memory | 3.5 MB idle; 20 to 28 MB under full load; about 19 KiB per open, idle WebSocket |
| Start-up | 3 ms to the first response; one 1.3 MB executable that needs only `libgc` |

**What this means for a real service:** the framework is unlikely to be the bottleneck. At 2 to 4 µs of server CPU per request, a typical handler that waits 1 to 5 ms on a database spends well under 1% of its time in Viper's HTTP layer. Throughput is then decided by the database, the network and the handler's own work. Where the handler's work is CPU-bound (building and encoding JSON, string processing), Viper runs it 1.1 to 3.4 times as fast as Go (see "The language itself" below).

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

| Endpoint | Viper | Go | Python | Viper vs Go |
|---|---|---|---|---|
| `/plaintext` | 981,099 (0.05 / 0.21; 14 MB) | 532,030 (0.09 / 0.57; 19 MB) | 27,361 (1.29 / 6.43) | 1.84x |
| `/json` | 943,077 (0.05 / 0.23; 12 MB) | 515,447 (0.09 / 0.59; 18 MB) | 26,443 (1.55 / 7.05) | 1.83x |
| `/items` | 616,747 (0.09 / 0.38; 16 MB) | 251,514 (0.18 / 1.09; 19 MB) | 17,678 (1.55 / 10.91) | 2.45x |
| `/users/42` | 897,316 (0.06 / 0.30; 16 MB) | 472,910 (0.10 / 0.65; 18 MB) | 24,428 (1.45 / 7.08) | 1.90x |
| `POST /orders` | 880,170 (0.06 / 0.32; 14 MB) | 294,237 (0.14 / 1.02; 18 MB) | 19,090 (1.67 / 11.65) | 2.99x |

**256 connections**

| Endpoint | Viper | Go | Python | Viper vs Go |
|---|---|---|---|---|
| `/plaintext` | 959,525 (0.21 / 0.85; 27 MB) | 502,591 (0.37 / 1.97; 23 MB) | 27,964 (3.80 / 17.29) | 1.91x |
| `/json` | 965,126 (0.21 / 0.85; 28 MB) | 488,599 (0.38 / 2.03; 23 MB) | 24,681 (4.10 / 19.13) | 1.98x |
| `/items` | 527,617 (0.38 / 1.14; 27 MB) | 235,700 (0.77 / 5.23; 24 MB) | 16,907 (4.02 / 37.20) | 2.24x |
| `/users/42` | 853,844 (0.24 / 1.01; 26 MB) | 461,751 (0.41 / 2.04; 25 MB) | 23,397 (3.85 / 22.24) | 1.85x |
| `POST /orders` | 857,771 (0.24 / 0.90; 20 MB) | 275,525 (0.64 / 4.31; 24 MB) | 18,272 (3.09 / 16.74) | 3.11x |

The Python server dropped a few hundred connections at 256 on every endpoint; its rows count only successful requests.

On the small endpoints, six CPUs are not the server's limit: it keeps about five of them busy, and the load generator and the kernel's loopback work on the other six set the rate. The per-core figures below are the better measure of the server.

**What to read from this:**
- **Small requests.** On requests where the server does little, Viper serves about twice Go's rate here, and 2.4 to 2.8 times per core, with lower latency at both percentiles. The difference is the cost of the HTTP layer itself: one read and one write per request, parsing in place, and switching between connections.
- **`/items`.** Here the handler's own work dominates: formatting 20 records and encoding 1.1 KB of JSON. Viper is 2.2 to 2.8 times Go's rate, with half its median latency and a quarter of its 99th percentile.
- **`POST /orders`**, which decodes JSON into structs, is 3 times Go's rate.
- **Tails.** Viper's 99th-percentile latency is the lowest of the three on every row.
- **Python** is 15 to 40 times slower than either.

## Per core

Each server on a single logical CPU (`VIPER_WORKERS=1`, `GOMAXPROCS=1`), 64 connections, best of 3 runs (`bench/http/one_core.py`). The CPU time per request is one second divided by the rate.

| Endpoint | Viper | Go | Viper vs Go | CPU per request: Viper / Go |
|---|---|---|---|---|
| `/plaintext` | 458,331 | 189,260 | 2.42x | 2.2 / 5.3 µs |
| `/json` | 526,076 | 189,382 | 2.78x | 1.9 / 5.3 µs |
| `/items` | 278,452 | 97,875 | 2.84x | 3.6 / 10.2 µs |
| `/users/42` | 433,433 | 174,870 | 2.48x | 2.3 / 5.7 µs |
| `POST /orders` | 348,768 | 116,596 | 2.99x | 2.9 / 8.6 µs |

These rates include the kernel's share of loopback networking, which a real network would replace with its own costs; it is now most of a small request's cost. `/plaintext` measured below `/json` in this run; in interleaved runs the two are level, at about 520,000.

**Realistic headers.** A browser sends a dozen headers, about 600 bytes. The server parses them without allocating, so with them `/plaintext` runs at about 85% of its bare rate on one CPU, the difference being mostly the kernel copying the larger request (D88).

**For sizing:** take the server CPU per request from this table, add the handler's own work, and divide into the cores available. Leave headroom: these runs keep every core fully busy, which a production service should not.

Six logical CPUs gave about three times the one-CPU rate, not six. Those six are three physical cores with two threads each, and the load generator was busy on the other six. Expect closer to linear scaling across physical cores.

## Memory

| | Viper | Go | Python |
|---|---|---|---|
| Idle server | 3.5 MB | 11.0 MB | 17.9 MB |
| Peak under load (256 connections) | 20 to 28 MB | 23 to 25 MB | 23 to 30 MB |
| Executable | 1.3 MB, needs only `libgc` at run time | 9.8 MB, static | the interpreter |
| Start-up to first response | 3 ms | 4 ms | 33 ms |

- **Under load.** Viper's peak is set mostly by the collector: it allocates at least 4 MiB between collections (`VIPER_GC_MIN_ALLOC`, the same minimum as Go's heap), so a busy server sits a few MB above its live data. Setting the variable lower trades memory for collection time.
- **Idle WebSockets.** `bench/ws_idle/` opened 50,000 WebSocket connections in 2.6 s and held them idle, and all 50,000 still echoed afterwards. The server grew from 3.6 MB to 927 MB: **18.9 KiB per connection**, almost all of it the touched pages of each connection's task stack.
  - **Rough capacity:** 1 GB of memory holds about 53,000 idle connections.
  - **Compared with earlier:** 11.3 KiB at milestone 10, 15.5 KiB after the performance plan. Building the runtime with link-time optimisation (D87), which made everything 2% to 6% faster, put one more stack page under each parked connection: the same source built without it uses 14.8 KiB (D88).

## The language itself

Handlers do more than the HTTP layer. These are the standard benchmarks in `bench/` (best of 3 runs, [bench/README.md](../bench/README.md)), with the ones most relevant to web services first:

| Benchmark | What it exercises | Viper | Go | Python | Viper vs Go |
|---|---|---|---|---|---|
| `json_orders` | Typed JSON encode and decode of 20,000 orders, ten times | 0.239 s | 0.710 s | 3.714 s | 2.97x faster |
| `word_count` | Splitting, lower-casing, string-keyed dicts | 0.387 s | 0.829 s | 2.437 s | 2.14x faster |
| `sort` | Sorting a million records with a key function | 0.587 s | 1.212 s | 2.192 s | 2.07x faster |
| `spawn` | Starting and joining 200,000 tasks | 0.040 s | 0.046 s | 1.292 s | 1.14x faster |
| `dispatch` | 10 million calls through trait objects | 0.010 s | 0.033 s | 0.617 s | 3.39x faster |
| `binary_trees` | Allocation and collection | 0.646 s | 0.946 s | 24.000 s | 1.46x faster |
| `fib` | Function calls | 0.022 s | 0.037 s | 0.686 s | 1.72x faster |
| `sieve` | List indexing and appends | 0.055 s | 0.060 s | 2.502 s | 1.09x faster |

Viper is ahead of Go on all eight, and 3.7 to 63 times faster than CPython. The weak spot is memory in allocation-heavy programs: `binary_trees` peaks at 65 MB against Go's 40 MB, and `sort` at 149 MB against 75 MB. The cause is object layout, not collection: each object carries an 8-byte header and is rounded to 16-byte granules, and a list of structs holds pointers to them where Go stores the values inline.

## What these numbers do not cover

- **One machine, loopback, client and server together.** A real network adds latency and its own CPU costs, and production servers have more cores and different caches.
- **No TLS.** The servers support HTTPS (`rustls` for Viper), but these runs are plain HTTP. TLS adds a per-connection handshake and per-byte encryption on both sides alike.
- **No database.** The PostgreSQL driver is not part of these benchmarks. With a database, its latency usually decides a request's time.
- **Python's baseline.** Python is the standard library's threaded server, the weakest option there is; an ASGI framework under uvicorn would do several times better, though still far from Go or Viper.
- **Closed-loop load.** Each connection waits for its response before sending the next request, which understates tail latency under overload compared with an open-loop generator at a fixed arrival rate.
- **HTTP/1.1 only**, with keep-alive. Viper does not speak HTTP/2.

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
