# Web performance: what to expect

A rough guide to what a Viper web service can do, measured against the same service written in Go (`net/http`) and in Python (the standard library's HTTP server), and against Node.js and Python web frameworks (Fastify, Express, FastAPI, Flask). The figures are from one laptop and a loopback network. Treat them as orders of magnitude and ratios, not as promises for production hardware. Measured on 2026-11-19 (the framework comparison on 2026-11-21), after the [performance plan](09-performance-plan.md) and the [final optimisation plan](10-final-optimization-plan.md).

## In short

| Question | Rough answer |
|---|---|
| How much CPU does the server itself cost per request? | About 2 µs for a small request or response; about 3.6 µs to build and encode a 1 KB JSON list. 35% to 40% of Go's cost, and 1/40th of Python's |
| How many requests per second per core? | 430,000 to 530,000 small requests; about 280,000 for a handler that builds and encodes a 20-record list |
| On a 3-core (6-thread) slice of a laptop? | About 960,000 small requests per second (the load generator's limit on this machine, not the server's), or 530,000 of the 20-record list |
| Added latency at 256 concurrent connections | Median 0.2 ms, 99th percentile 0.9 ms for small responses; 0.4 / 1.1 ms for the 20-record list. Go's 99th percentile is 2 to 5 times higher |
| Memory | 3.5 MB idle; 20 to 28 MB under full load; about 19 KiB per open, idle WebSocket |
| Start-up | 3 ms to the first response; one 1.3 MB executable that needs only `libgc` |
| Against Node.js and Python frameworks | On four cores, on `/users/{id}`: 2.9 times Fastify's request rate, 15 times Express's, 12 times FastAPI's and 28 times Flask's, using 1.7 MB at rest where they use 87 to 184 MB (see "Against web frameworks" below) |

**What this means for a real service:** the framework is unlikely to be the bottleneck. At 2 to 4 µs of server CPU per request, a typical handler that waits 1 to 5 ms on a database spends well under 1% of its time in Viper's HTTP layer. Throughput is then decided by the database, the network and the handler's own work. Where the handler's work is CPU-bound (building and encoding JSON, string processing), Viper runs it 1.1 to 3.4 times as fast as Go (see "The language itself" below).

## Against web frameworks

The figures above compare Viper with Go and with Python's standard-library server. Most services are written with a framework, so `bench/compare/` runs one REST API and WebSocket echo on six servers: Viper, Go (`net/http` with gorilla/websocket), Node.js 20 with Fastify 5 and with Express 4 (both with `ws`), and Python 3.13 with FastAPI on Uvicorn (uvloop, httptools) and Flask on Gunicorn. Measured on 2026-11-21, on the same laptop.

**The setup.** Each server is pinned to four logical CPUs and runs one worker per CPU, as it would be deployed: Go and Viper schedule across cores themselves, Node runs a `node:cluster` process per core, and Uvicorn and Gunicorn run four worker processes (Gunicorn with 16 threads each). The load generator (`bench/compare/load/main.go`) runs on the other eight CPUs and opens its connections before timing starts. Memory is the proportional set size (PSS) summed over the server's process tree, so forked workers that share pages are not counted twice; it reads lower than the resident size used in the tables above. Each figure is one 5-second run. The charts are in `bench/compare/report/` (`python3 bench/compare/report/build.py` writes `index.html`), and [bench/README.md](../bench/README.md) has the commands.

**Requests per second at 256 connections**, with median and 99th-percentile latency in milliseconds:

| Endpoint | Viper | Go net/http | Node Fastify | Node Express | Python FastAPI | Python Flask |
|---|---|---|---|---|---|---|
| `GET /json` | 1,060,955 (0.21 / 0.83) | 375,797 (0.55 / 2.74) | 258,995 (0.80 / 2.70) | 49,971 (4.02 / 16) | 75,648 (3.29 / 5.00) | 28,065 (6.93 / 44) |
| `GET /users/{id}` | 788,517 (0.28 / 1.04) | 378,875 (0.57 / 2.15) | 268,369 (0.86 / 1.76) | 52,467 (4.09 / 17) | 64,014 (3.69 / 6.08) | 28,416 (7.41 / 46) |
| `GET /items` | 393,514 (0.54 / 1.77) | 212,805 (1.01 / 4.12) | 186,975 (1.31 / 2.40) | 43,839 (4.76 / 17) | 50,688 (4.86 / 6.97) | 25,231 (8.56 / 53) |
| `POST /orders` | 638,424 (0.35 / 1.15) | 249,337 (0.84 / 4.03) | 191,662 (1.23 / 2.97) | 36,753 (5.52 / 21) | 49,702 (4.69 / 7.99) | 24,865 (8.79 / 51) |

**Latency at a fixed request rate** on `GET /users/42` (median / 99th percentile, ms). A request that had to wait for the server's previous answer is timed from when it was due, so falling behind shows as latency. A server stops at the first rate it cannot hold to within 5% without errors:

| Server | 1,000/s | 10,000/s | 50,000/s | 100,000/s | 200,000/s | 400,000/s |
|---|---|---|---|---|---|---|
| Viper | 0.07 / 0.25 | 0.04 / 0.10 | 0.04 / 0.14 | 0.02 / 0.11 | 0.01 / 0.24 | 0.01 / 1.34 |
| Go net/http | 0.07 / 0.17 | 0.06 / 0.15 | 0.02 / 0.34 | 0.02 / 0.45 | 0.02 / 1.67 | fell behind (378,571/s) |
| Node Fastify | 0.11 / 0.27 | 0.09 / 0.29 | 0.03 / 0.28 | 0.02 / 0.85 | 0.15 / 20 | fell behind (256,726/s) |
| Node Express | 0.20 / 0.52 | 0.15 / 3.16 | fell behind (45,560/s) | | | |
| Python FastAPI | 0.37 / 0.65 | 0.15 / 0.62 | 0.73 / 178 | fell behind (53,174/s) | | |
| Python Flask | 0.48 / 1.29 | 1.23 / 19 | fell behind (22,214/s) | | | |

**Memory, WebSockets and CPU:**

| Server | Processes | Idle MB | Peak MB under load | Req/s at 1,024 connections | WebSocket echo, msg/s (p50 / p99 ms) | 10,000 idle WebSockets | Requests per CPU-second | Cores used |
|---|---|---|---|---|---|---|---|---|
| Viper | 1 | 1.7 | 54 | 572,945 | 1,230,056 (0.19 / 0.50) | 186 MB, 18.9 KB each | 208,156 | 3.5 |
| Go net/http | 1 | 9.9 | 45 | 308,063 | 862,309 (0.23 / 1.07) | 201 MB, 19.6 KB each | 79,271 | 3.8 |
| Node Fastify | 5 | 183.6 | 389 | 220,236 | 480,834 (0.49 / 1.81) | 276 MB, 9.2 KB each | 56,988 | 4.0 |
| Node Express | 5 | 153.6 | 439 | 49,369 | 481,801 (0.49 / 1.85) | 263 MB, 11.3 KB each | 12,242 | 3.7 |
| Python FastAPI | 6 | 163.3 | 178 | 52,772 | 208,420 (1.16 / 2.41) | 530 MB, 37.6 KB each | 15,025 | 4.0 |
| Python Flask | 5 | 86.5 | 105 | 26,659 | no WebSocket support | | 6,680 | 4.0 |

**What to read from this:**
- **Throughput.** Viper answers 1.8 to 2.8 times Go's request rate, 2.1 to 4.1 times Fastify's, 9 to 21 times Express's, 7.8 to 14 times FastAPI's and 16 to 38 times Flask's, depending on the endpoint.
- **CPU.** Each CPU-second of Viper's time answers about 208,000 requests: 2.6 times Go, 3.7 times Fastify, 14 times FastAPI and 17 times Express. This is the figure for sizing hardware. Viper used 3.5 of its 4 cores, so its rates here are set partly by the load generator; they are a floor.
- **Load bearing.** Viper held 400,000 requests per second with a 1.3 ms 99th percentile. Go and Fastify held 200,000 (1.7 ms and 20 ms) and fell behind at 400,000; FastAPI held 50,000 with a 178 ms tail; Express and Flask held 10,000. With 1,024 connections open, Viper's rate drops by a third from its peak, and stays 1.9 times Go's.
- **Memory.** Viper is one process using 1.7 MB at rest and 54 MB at its peak; Go uses 10 and 45 MB. The Node and Python servers run one process per core, each with its own runtime, and use 87 to 184 MB before they serve a request.
- **Idle WebSockets.** Each idle connection costs Node less than Viper (9.2 KB for Fastify against 18.9 KB, almost all of Viper's being the touched pages of the connection's task stack). Node starts about 180 MB higher, so Viper uses less memory in total up to about 20,000 idle connections. FastAPI's connections cost twice Viper's.
- **Low rates.** At 1,000 requests per second every server answers in well under a millisecond at the median. The differences above matter for capacity, tail latency under load and memory, not for a lightly loaded service.

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

## With metrics and the access log

The tables above are servers with nothing switched on. A production server usually has metrics and an access log. Measured at 256 connections, 3 alternating runs of 5 s each (`bench/http/server.vi PORT metrics` or `observed`):

| Endpoint | Plain | Metrics on | Metrics and access log |
|---|---|---|---|
| `/plaintext` | 1,087,529 | 1,100,665 (+1.2%) | 599,063 (-42%) |
| `/json` | 1,090,881 | 1,089,442 (-0.1%) | 745,783 (-31%) |
| `/items` | 598,631 | 590,693 (-1.3%) | 396,413 (-35%) |
| `/users/42` | 1,008,261 | 998,460 (-1.0%) | 635,582 (-33%) |
| `POST /orders` | 966,384 | 965,032 (-0.1%) | 569,138 (-40%) |

- **Metrics cost two clock reads per request** and no allocation or system call (`bench/http/counts.py`). Counters are per-thread shards of atomics, so threads never wait on each other.
- **The access log costs most of the difference**: a request ID, a line with seven fields and its formatting, about 30 allocations and four clock reads per request, though no extra system calls since lines are buffered. At these rates that is a third or more of the server's work; for a handler that does real work (a database query) it is a few microseconds. Making it cheaper is planned.
- The two columns' plain rates come from separate runs, so the plain figures differ slightly.

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
- **Python's baseline.** In the tables after "Against web frameworks", Python is the standard library's threaded server, the weakest option there is. FastAPI under Uvicorn is measured in "Against web frameworks": two to three times the standard library's rate, still far from Go or Viper.
- **Closed-loop load.** Except for the fixed-rate table in "Against web frameworks", each connection waits for its response before sending the next request, which understates tail latency under overload.
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

The framework comparison needs Node.js and the frameworks installed first ([bench/README.md](../bench/README.md), "Against web frameworks"):

```bash
python3 bench/compare/run.py
```

Go must be installed for the comparisons (`~/.local/go/bin` is searched too). `bench/profile/` has a small sampling profiler for finding where a handler's time goes on machines without `perf`.
