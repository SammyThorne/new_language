# Release notes

Viper is a compiled, Python-shaped language for web backends. `viper --version` reports 1.1.0: the first release, 1.0.0, with the API documentation of [1.1.0](#new-in-110) added. This page says what the language has, what it does not, and how to install it. The state of each part, with the reasoning behind the choices, is in the [roadmap](06-roadmap.md) and the [decision log](08-decisions-and-risks.md); [Viper by example](examples.md) teaches the language, and every program in it is compiled and run by the test suite.

## What the language has

**The language.**
- Python's layout and much of its syntax: functions, `if`/`elif`/`else`, `while`, `for`, `match` (as a statement, and as a value on the right of `=` or `return`), `with`, comprehensions for lists, dicts and sets, f-strings with `!r`, raw strings, lambdas, nested functions, one-line blocks (`if done: return`), constants worked out when compiling, and `del`.
- Static types written as Python type hints, with local inference. Every parameter and return type is written; variables take the type of their first value. There is no implicit conversion between numbers.
- Structs and `impl` blocks, enums with fields, traits with default methods and required traits, trait objects (taken apart again with `case Circle(c):`), `@derive` for `Eq`, `Hash`, `Display`, `Copy`, `Ord` and `Json`, `@frozen` structs.
- Generics for functions, structs, enums, `impl` blocks and methods (a method can have type parameters of its own), with bounds, compiled once per set of types.
- Errors are values: `Result[T]`, `T?` for optional values, `?` to pass an error on, and a panic (with a backtrace of your functions and lines) for what cannot be recovered.
- Types: `int` (64 bits, wrapping), `float`, `bool`, `str`, `list`, `dict` (insertion ordered), `set`, tuples, `bytes`, sized numbers for C, optionals, and the immutable `tuple[T, ...]` and `frozendict` for sharing between tasks.
- Modules and packages (`import`, `pub`/`priv`), a project manifest with git and path dependencies, C functions through `@extern`.

**The standard library.** `io` and `os` (files, standard streams, the environment), `json` (typed encoding and decoding with `@derive(Json)`, and a dynamic `json.Value`), `http` (server, client, routes, typed handlers, WebSockets, TLS), `re` (regular expressions that match in linear time), `time`, `log`, `postgres` (a driver with pooling, transactions and typed rows), and about thirty more methods of `str`, `list` and `dict` than a minimal core would have.

**Concurrency.** `spawn` starts a task; tasks are fibers scheduled over one worker thread per core. `Channel`, `Mutex` (reached only through `with m.lock() as value:`), `Atomic`. The compiler rejects programs that would share a changeable value between tasks, and says how to fix them.

**The web stack.** Handlers can be written with annotations, `@http.get("/todos/{id}")`, and typed parameters that the compiler binds from the path, query, headers, cookies and a JSON body, with `400`s generated for bad input. WebSocket endpoints with `@http.ws`. HTTPS with `rustls`. Graceful shutdown on SIGTERM.

**Tooling.** `viper run`, `build`, `check`, `test` (for `@test` functions), `fmt`, `fetch`, `explain` (for an error code) and `lsp`. The language server gives diagnostics, hover, go to definition, completion and formatting. Editor support for VS Code, Vim, Neovim, Helix and Zed is in `editors/`. The messages point at the source, suggest the nearest name, and every code has an entry in the [error reference](errors.md).

**Performance.** On this repository's benchmarks every program runs faster than Go's version (between 1.1 and 3.0 times), and the HTTP endpoints serve 1.8 to 3.1 times Go's request rate at 256 connections with about 40% of its CPU per request. Against Node.js and Python frameworks on four cores, Viper serves 2.9 times Fastify's request rate and 12 to 28 times those of FastAPI, Express and Flask, holds 400,000 requests per second with a 1.3 ms 99th percentile where Go and Fastify fall behind, and uses 1.7 MB at rest against their 87 to 184 MB. Idle WebSocket connections cost about 19 KiB each. A small program's executable is 0.5 MB and the benchmark web server's 1.3 MB. The numbers, how they were measured and what limits them are in [web-performance.md](web-performance.md) and [bench/README.md](../bench/README.md). `bench/run.py --check` and `bench/http/counts.py --check` fail on a regression.

## Since 1.1.0: signing in with Google, and production web features

Not yet in a numbered release; `viper --version` still says 1.1.0.

**The language is now called Viper** (D109). The command is `viper`, source files end in `.vi`, environment variables start with `VIPER_` (`VIPER_LOG`, `VIPER_WORKERS`, ...), and projects use `viper.toml` and `viper.lock`. Rename existing `.wb` files and `weby.toml` to match; the old names are not read.

**Logs and requests** ([production web plan](14-production-web-plan.md), milestone 1, D105):
- `log` functions take `fields` (`log.info("saved", {"order": id})`), written as `name=value`, or as one JSON object per line after `log.set_format("json")` or with the `LOG_FORMAT` variable. `log.with_fields(...)` gives a logger with fields of its own.
- `http.request_log()` gives each request an ID (the client's `X-Request-Id` or a new one), returns it, logs one line per request, and adds the ID to every line logged while the request is handled, including from tasks it spawns.
- While a server runs, log lines to a pipe or file are written in blocks rather than one system call each.
- `req.client_ip()` with `server.trusted_proxies`, for servers behind a proxy; `req.body()` as `bytes`; `http.binary(...)` for responses of any bytes; `os.setenv`.

**CORS** (milestone 2, D106): `http.cors(origins=[...])` lets pages on other origins call the server from a browser. It answers preflights, adds the headers to responses (error responses too), supports credentials, exposed headers and a function that decides origins, and works on a group of routes. A lambda can now be passed where an optional function is expected.

**Static files** (milestone 3, D107): `server.static("/assets", "public")` serves a directory from the runtime, binary-safe, with content types, `ETag` and `Last-Modified` (304s and 412s), `Cache-Control` (`max_age`, `immutable`), single ranges (206, 416, `If-Range`), pre-compressed `.br` and `.gz` files, directory indexes, and large files sent in pieces so memory stays flat. `fallback="index.html"` serves a single-page app. Paths that try to leave the directory (`..` in any spelling, encoded separators, symbolic links) and dotfiles are 404s.

**Compression** (milestone 4, D108): `server.use(http.compress())` gzips text, JSON, JavaScript and SVG bodies of 1 KB or more for clients that accept it, with `Vary: Accept-Encoding` and weak ETags. The HTTP client (`http.get`, `post`, `fetch`) now asks for gzip and decodes it. The runtime gains one dependency, `miniz_oxide`.

**Dates and times** (milestone 5, D110): a new `datetime` module with `DateTime` (an instant, shown in a zone), `Date`, `Duration` and `Zone`. `datetime.parse` reads RFC 3339 and ISO 8601 and says which character it did not expect; `format_iso`, `format` (a strftime subset) and `http_date` write them. Time zones come from the system's zoneinfo (or `TZDIR`), including the rules past the end of each table, with gaps and repeated hours resolved as Go and Python do. Durations use Go's syntax (`parse_duration("1h30m")`), `datetime.sleep` takes one, and `http.fetch` has a `timeout=`. All three types are JSON strings, show as `date-time`, `date` and string schemas in the OpenAPI document, and `postgres.query_as` reads `timestamptz`, `timestamp` and `date` columns into them. `datetime.set_clock_for_tests` fixes `now()`. The prelude gains `JsonString`: implement it to give any type a JSON string form. Two compiler fixes came with it: a parameter default that builds a struct with field defaults no longer reports the field as missing, and `json.decode[datetime.Date](text)` (a module-qualified type argument) is accepted.

**UUIDs** (milestone 6, D111): a new `uuid` module. `uuid.v4()` is random; `uuid.v7()` is time-ordered, so database keys made together stay together, and each one sorts after every one before it in the process. `uuid.parse` accepts either case and says which character was wrong. A `Uuid` is a JSON string and `format: uuid` in OpenAPI. Handler parameters of type `uuid.Uuid`, and of any type with a string form (`datetime.Date`, or your own `JsonString` type), are read from the path or the query, with a 400 for bad input. `http.request_log()` now makes its request IDs with `uuid.v7()`.

**Configuration** (milestone 7, D112): a new `config` module. `config.load[Settings]()` fills a struct deriving `Json` from environment variables, `PORT` for field `port`, with an optional prefix. Numbers, `yes`/`no` booleans, durations, UUIDs and comma-separated lists are read from the text, and a `.env` file supplies what the environment does not set. A deployment that gets several variables wrong is told about all of them in one error: `config: DATABASE_URL is required; PORT must be an integer, got "80a"`. `config.load_env()` copies a `.env` file into the environment instead. The file's syntax is documented exactly, with single and double quotes, multi-line values and `export`, and a malformed line is an error naming its line. `config.Secret` holds a password or key that prints, logs and encodes as `***` until `reveal()` is called. `os.environ()` lists the environment.

**Metrics and health** (milestone 8, D113): `server.metrics()` serves Prometheus metrics at `/metrics`: requests by method, route pattern and status, a histogram of their durations, response sizes, connections, errors, and the process's memory, CPU, heap and tasks. It needs `METRICS_USER` and `METRICS_PASSWORD` (Basic authentication), or `public=True`, and refuses to start without one or the other. A server that does not call it pays nothing for it. A new `metrics` module gives programs counters, gauges and histograms of their own (`with db_time.time():`), with at most 1000 sets of label values each. `server.health(checks={...})` adds `/healthz` and `/readyz`, which runs the checks concurrently with a two-second limit; after SIGTERM, `/readyz` fails at once and the server goes on serving for `server.drain_delay` seconds, so a load balancer can take it out first. `req.route()` gives the pattern of the route that took a request, and `http.request_log()` logs it as `route`.

**The metrics dashboard** (milestone 9, D114): `server.metrics()` also serves a page at `/metrics/dashboard`, behind the same password, that charts the last hour: requests per second by status, latency percentiles, requests in flight, connections, tasks and memory, each route's totals, the recent server errors and the slowest requests with their request IDs, and the program's own metrics. It is embedded in the runtime, with no CDN or chart library, follows light and dark mode, works on a phone, and is left out of programs that do not serve metrics; `dashboard=False` turns it off. `viper hash-password` prints a PBKDF2 hash to put in `METRICS_PASSWORD_HASH` in place of the password. Ten wrong passwords in a minute lock a client out for a minute, and credentials sent over plain HTTP from another machine are warned about once. `examples/metrics_dashboard` shows it in a small service.

**Traces** (milestone 10, D115): the new `trace` module sends spans to an OpenTelemetry collector as OTLP/JSON. After `trace.init("service")`, or `http.request_log(trace=True)` with `OTEL_EXPORTER_OTLP_ENDPOINT` set, every request a server answers is a span named by its route, continuing the caller's trace from `traceparent`; every `http.fetch` is a child span that sends `traceparent` on; every PostgreSQL query is a span named by its operation; and `with trace.span("name"):` adds spans of the program's own. Log lines in a trace carry `trace_id` and `span_id`. Sampling is by ratio and follows the caller's choice; spans are batched on a thread of their own and dropped, and counted on `/metrics`, rather than slowing the service when the collector is down.

**Request size limits** (milestone 11, D116): `max_body` can be set per route (`server.post(path, handler, max_body=...)` or `@http.post(path, max_body=...)`) and per group, as well as on the server. Bodies over 64 KiB are read only when the handler asks for them, against the route's limit, so one route that takes large uploads no longer makes every route accept them. A body past its limit is always answered 413, even if the handler ignores the error. `Expect: 100-continue` is answered only when the handler reads the body. A body the handler does not read is drained, up to 1 MiB, so the connection stays open, and a connection that closes lets the client read its response first. New limits on the number of headers (`server.max_headers`, 100: 431) and the length of the URL (`server.max_url`, 8 KiB: 414).

**Forms and uploads** (milestone 12, D117): `req.form()` reads urlencoded and multipart forms, with `get`, `all`, `file` and `files`. Annotated handlers take `http.Form[T]` for the text fields decoded into a type (every bad field named in one 400) and `http.Upload`, `http.Upload?` or `list[http.Upload]` for files, which have `bytes()`, `text()` and `save(path)`. File names are reduced to a safe base name. Files over 1 MiB wait in `server.upload_dir` and are removed when the request ends. New limits `server.max_files`, `max_fields` and `max_part_headers`. The API documentation describes form bodies, and its "Try it out" can upload files. `test_request` takes `form=` and `files=` (made with `http.test_file`). A form parameter with a JSON body parameter is the new error E0343. `config.load` now reads `on` and `off` as booleans.

**Timeouts and cancellation** (milestone 13, D118): routes, groups and servers take a `timeout`, and annotations take `timeout=`. A handler past its timeout gets a 503 with `Retry-After`, and the tasks it spawned stop with it. A client that hangs up cancels its handler, which is counted as `client_closed`. Cancellation stops every wait: `sleep`, channel receives, client calls, `postgres` queries (the server is told to stop the query too) and the database pool. `http.fetch` takes `connect_timeout=`, and its errors name the limit that ran out. `req.deadline()`, `req.cancelled()` and `http.check_cancelled()` are there for code that checks for itself.

**Rate limiting** (milestone 14, D119): `http.rate_limit(per_second=20.0, burst=40)` or `http.rate_limit("100/minute")` limits each client, by `req.client_ip()` or a `key` function, with a token bucket. Refused requests get a 429 with `Retry-After`, shaped by `on_error`; every response carries the IETF draft's `RateLimit-Policy` and `RateLimit` headers. Routes and annotations take `rate_limit="5/minute"`, checked by the compiler and listed as a 429 in the API documentation. At most 100,000 clients are tracked per limit. The metrics dashboard's lock-out after failed sign-ins now uses the same limiter.

**Streams and Server-Sent Events** (milestone 15, D120): `http.stream(req, body, content_type="text/csv")` sends a response while `body` writes it, in chunks of 16 KiB with `flush` to send sooner, so exports and logs of any size take flat memory. `http.sse(req, handler)` serves Server-Sent Events to a browser's `EventSource`, with `event`, `id` and `retry`, a heartbeat every 15 s, and `events.last_id()` for resuming. Writes fail with `http.StreamClosed` once the client has gone; a function that fails leaves the body cut short so the client knows. Streams are not compressed and outlive their route's timeout. In tests, `server.handle` collects a stream into the response body. The metrics dashboard now updates live over Server-Sent Events, and polls where that fails.

**HTML templates** (milestone 16, D121): the `template` module is a small Jinja-like language for pages: `{{ }}`, `if`, `for`, `set`, `include`, `extends` with blocks, and the filters `upper`, `lower`, `title`, `trim`, `length`, `default`, `join`, `date`, `json`, `url` and `raw`. `template.load_dir("templates", reload=...)` checks every template when it loads, and `http.render(pages, "home.html", context)` makes the response. Output is HTML-escaped by where it sits: a `{{ }}` in a `<script>`, a `<style>` or an `on...=` attribute must say `| json`, a URL attribute whose value comes to a `javascript:` or other unknown scheme becomes `#unsafe`, and a value in an unquoted attribute or a tag name is refused. A field that is not there is an error naming the template, line and expression, never a blank. A 1000-row table renders in about 1.2 ms. `examples/recipes` is a small site.

**Signing in with Google** ([plan](13-google-oauth-plan.md), D100 to D104). The new `auth` module signs users in with their Google accounts from the credentials file the Google Cloud console makes, as Passport, Authlib or NextAuth do in other languages:

- `provider.install(server)` adds the login, callback and logout routes and a signed session cookie. It uses the authorization code flow with PKCE, `state` and `nonce`, and checks the ID token's RS256 signature against Google's keys, which are cached.
- `@auth.required` on a route, or a handler parameter of type `auth.User`, protects the route; `auth.User?` is the user when there is one. Signed-out browsers are sent to sign in and brought back, and API clients get a 401. The compiler checks the uses (error E0342), and the API documentation lists the session cookie and the 401 for protected routes.
- `auth.protect(handler)` and `auth.required` as middleware protect call-style handlers and groups.
- Policies: `allowed_domains` (Google Workspace), `allowed_emails`, verified email by default, and `on_login` to record, change or refuse users.
- Tests sign requests in with `auth.google_for_tests()` and `test_cookie`, without contacting Google. `examples/google_login` is a small app with its tests.

Also new on the way:
- The `crypto` module: secure random bytes and tokens, SHA-256, HMAC-SHA256, constant-time comparison, base64 and base64url, and RS256 signature checks.
- In `http`: any number of cookies on a response (`set_cookie`, `delete_cookie`), `url_encode`, `url_decode`, `form_encode`, `post_form`, test requests with headers and a query string, `req.query_string()`, values middleware keeps with a request (`set_local`, `local`), and `server.security_scheme` for libraries that document their own security.
- The language server shows the docstring of `@auth.required` on hover.

Limits: only Google, and only sign-in: Google's access tokens are not kept for calling its APIs, and there are no other OpenID Connect providers, GitHub sign-in, server-side sessions, roles, or `auth.User` parameters in WebSocket handlers yet. The flow has been tested against fake Google servers; signing in with a real Google client is checked by hand (see the plan's milestone 6).

## New in 1.1.0

**API documentation** ([plan](12-openapi-docs-plan.md), D96 to D99). `server.docs("/docs")` serves an OpenAPI 3.1 document of the server's routes at `/docs/openapi.json` and `/docs/openapi.yaml`, and a documentation page at `/docs`, like FastAPI's `/docs` and Fastify's swagger plugins:

- The document is generated from what the compiler already knows: parameter and body types, the status, the response type, the `400`s it generates, and each handler's docstring (summary and description). Route annotations take `tags=`, `deprecated=`, `errors=` and `security=`; `docs` takes tag descriptions (`http.Tag`) and security schemes (`http.bearer`, `http.basic_auth`, `http.api_key`).
- The page is written for the project (46 KB of plain HTML, CSS and JavaScript in the runtime, nothing downloaded or loaded from elsewhere): operations by tag, schemas, a filter, "Try it out" with the `curl` command, an "Authorize" box, dark mode, and a phone layout.
- A program that does not call `docs` compiles to the same code and the same size as before; one that does is about 100 KB larger.
- `server.openapi_json(...)` returns the document as text.

Limits: routes registered in call style are listed with their method and path parameters only; a generic struct cannot be a handler's body or return type directly; the documentation describes security schemes but does not enforce them (middleware does); OAuth2 and OpenID Connect flows, file uploads and XML bodies are not described.

## What changed in the last round

The [release polish plan](11-release-polish-plan.md) closed the gaps a Python programmer meets in the first hour:

- syntax: one-line blocks, raw strings, `del`, constant expressions, `!r` and `!s`, `print(..., sep=, end=, flush=)`, `match` as a value;
- `str`, `list` and `dict` gained about thirty methods written in the language's own prelude, and the functions `ord`, `chr`, `reversed` and `list(...)`;
- `set[T]`, with literals, comprehensions, operators, methods and JSON;
- methods with type parameters of their own, and `case Circle(c):` on a trait object;
- a clear `panic: stack overflow` and exit status 101 instead of a segmentation fault;
- completion in the language server, a warning for unused imports, `viper explain`, and executables four to eight times smaller.

## Known limitations

These are choices made for this release, each with its reason in the decision log. None changes a program that compiles.

**Platform.** Linux on x86-64 only (D13). The context switch is assembly for that CPU, the poller is `epoll`, and the compiler is built with LLVM's x86 target. Windows and macOS run it in WSL 2 and a Lima VM through the install scripts; programs built there are Linux programs.

**Language.**
- No generic traits: iteration, `with` and the like are method protocols (`next`, `iter`, `enter`, `exit`; D34, D38). Traits cannot have associated types.
- A closure can read the enclosing function's variables but not assign them (D35). Comparison chains (`a < b < c`), `*args` and `**kwargs`, `global` and `nonlocal`, and raw f-strings are errors, each with a message that says what to write.
- `empty = []` needs a type hint, `items: list[int] = []`, because a type is not inferred from a later use (D94).
- No bytes literals (`b"..."`; write `"...".encode()`) and no immutable set (`frozenset`); share a `set` through a `Mutex` (D91, D93).
- A set prints and iterates in insertion order, where Python's sets have an order of their own, and its methods take sets, not any iterable.
- Strings are indexed by byte offset, so `len(s)` is the number of bytes (D22).
- Integers wrap on overflow (D16).

**Runtime.**
- There is no preemption: a task that never waits holds its worker thread until it does, and as many such tasks as there are cores stop the program's other tasks. Keep handlers short, or have a long computation wait now and then, with `time.sleep(0.001)` or a channel operation. A check at every loop iteration was measured and slows tight loops by 25% (D95).
- There is no blocking pool: file I/O and `@extern` calls run on the worker that makes them (D66).
- A panic in a task stops the task, not the program (a handler's becomes a `500`), but a `with` block it leaves does not run its `exit` (D64); mutexes it held are released. A stack overflow ends the whole program.
- The collector is Boehm's, conservative and not moving; memory for allocation-heavy programs can be above Go's (D86). Collections of structs hold pointers, not inline values.

**Tooling.**
- Debug information has line tables and function names but not variables or types (D67): a debugger can break, step and show a backtrace, not print locals.
- `viper fmt` keeps your line breaks; it does not fit lines to a width (D68). The language server checks the whole program on each change (D70).
- Dependencies come from git or paths with a lock file; there is no registry and no version solving (D71).
- Testing: the runtime has not been run under ThreadSanitizer, which needs a nightly Rust toolchain (D45). JSON and WebSocket conformance suites are run by hand, not in continuous integration (D81).

## Installing

On Debian or Ubuntu, `scripts/install.sh` installs everything the build needs and then the compiler; `scripts/install.ps1` (Windows, through WSL 2) and `scripts/install-macos.sh` (macOS, through a Lima VM) run it in a Linux environment. They are described in [building.md](building.md#installing-with-a-script). 

By hand, you need Linux on x86-64, LLVM 19, the Boehm collector (`libgc`), a C compiler to link with, and a Rust toolchain to build the compiler. [building.md](building.md) lists the packages and the steps:

```bash
make
```

```bash
make install
```

The first builds the compiler and the runtime library; the second puts the `viper` command in `~/.local/bin` (`PREFIX=... make install` for somewhere else). Then:

```bash
viper run hello.vi
```
