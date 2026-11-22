# Production web features: research and plan

This is a plan, written so that an agent with no other context can carry it out. **Milestones 1 to 24 are built; the decisions in section 5 are confirmed.** Each milestone in section 6 ends with a "Done" note once built.

It adds what a web service needs in production and that the language does not have yet:
- CORS, static files, compression, request IDs, structured logs and access logs.
- Dates, times, durations and UUIDs.
- Typed configuration from the environment and `.env` files.
- Metrics, a metrics dashboard behind a username and password, health checks and traces.
- Request size limits, forms and file uploads, timeouts and cancellation, rate limiting.
- Streaming responses, Server-Sent Events and HTML templates.
- SQLite, database migrations and background jobs.
- CSRF protection, generic OpenID Connect sign-in (Microsoft, Okta, Auth0) and GitHub sign-in.

The guide is what developers get from Express, FastAPI/Starlette, Go's `net/http`, Django and Rails, with fewer packages to choose between: everything here is in the standard library, documented in the same place, and costs nothing when a program does not use it.

Read first:
- [05-standard-library.md](05-standard-library.md): HTTP, "Route annotations", "How handler parameters are bound", `log`, `time`, `os`, `io`, `crypto`, `auth`.
- `stdlib/http.lang`: `Request`, `Response`, `Server` (`compose`, `dispatch`, `serve`, `docs`), middleware and groups.
- `crates/lang_rt/src/http.rs`: `serve_connection`, `read_request`, `lrt_http_respond`, `write_response`, `lrt_log_write`.
- `crates/lang_rt/src/docs.rs`: how the documentation viewer's files are embedded in the runtime and dropped by the linker when unused. The metrics dashboard does the same.
- `crates/lang_sema/src/check/http.rs`: how routes are checked and how `mount` binds parameters.
- [12-openapi-docs-plan.md](12-openapi-docs-plan.md) and [13-google-oauth-plan.md](13-google-oauth-plan.md): the two plans this one follows in form.
- The conventions in [11-release-polish-plan.md](11-release-polish-plan.md): commit and push per milestone, the gates, and section "Rules".
- [web-performance.md](web-performance.md) and `bench/http/counts.py`: the per-request costs that must not grow.

## 1. What the user gets

```python
import http
import config
import log

@derive(Json)
struct Settings:
    port: int = 8080
    allowed_origin: str = "http://localhost:5173"
    database_url: str

def main():
    # `.env` if there is one, then the environment, which wins: PORT, ALLOWED_ORIGIN, DATABASE_URL.
    settings = config.load[Settings]().unwrap()
    log.set_format("json")

    server = http.Server()
    server.use(http.request_log())                  # a request ID and one log line per request
    server.use(http.cors(origins=[settings.allowed_origin], credentials=True))
    server.use(http.compress())
    server.use(http.rate_limit(per_second=20.0, burst=40))
    server.static("/assets", "public")              # ETags, 304s, content types, no `..`
    server.health()                                 # /healthz and /readyz
    server.metrics()                                # /metrics, and a dashboard at /metrics/dashboard
    server.mount(App(settings))
    server.listen(f"0.0.0.0:{settings.port}").unwrap()
```

The metrics routes need a username and a password. They come from the environment, or from the `.env` file that `config.load` reads:

```sh
# .env (never committed)
METRICS_USER=admin
METRICS_PASSWORD=correct-horse-battery-staple
DATABASE_URL=postgres://app@localhost/app
```

Without them, `server.metrics()` stops the program at startup with a message that says what to set. It does not serve the metrics to everyone. The dashboard is one page with no external scripts. It shows:
- requests per second, error rate and latency percentiles over the last hour;
- a table of routes with their traffic and latency;
- the most recent server errors and the slowest recent requests, each with its request ID;
- memory, connections and tasks.

Prometheus scrapes `/metrics` with the same credentials.

Routes gain options, parameter types and response kinds:

```python
import datetime
import uuid

@derive(Json)
struct Photo:
    id: uuid.Uuid
    title: str
    taken: datetime.DateTime            # "2026-11-20T09:30:00Z" in JSON, `format: date-time` in OpenAPI

@derive(Json)
struct PhotoForm:
    title: str
    public: bool = False

impl App:
    @http.post("/photos", max_body=50_000_000, timeout=60.0)
    def upload(self, form: http.Form[PhotoForm], image: http.Upload) -> Result[Photo]:
        path = f"uploads/{uuid.v7()}"
        image.save(path)?
        return Ok(Photo(uuid.v7(), form.title, datetime.now()))

    @http.get("/events")
    def events(self, req: http.Request) -> Result[http.Response]:
        return http.sse(req, lambda events: self.feed.follow(events))

    @http.get("/")
    def home(self, req: http.Request) -> Result[http.Response]:
        return self.pages.render("home.html", {"photos": self.photos.recent()})   # auto-escaped
```

Background work and the database:

```python
import sqlite
import migrate
import jobs

db = sqlite.open("app.db").unwrap()
migrate.apply(db, "migrations").unwrap()        # 0001_create_photos.sql, ... in order, once each
jobs.every(datetime.minutes(5), lambda: refresh_feed(db))
jobs.cron("0 3 * * *", lambda: prune_old(db), zone="Europe/London")
```

Signing in with providers other than Google:

```python
microsoft = auth.microsoft(tenant="common", client_file="microsoft_client.json")
github = auth.github(client_id=os.getenv("GITHUB_CLIENT_ID").unwrap(), client_secret=os.getenv("GITHUB_CLIENT_SECRET").unwrap())
auth.install_all(server, [google, microsoft, github])   # /auth/{provider}/login, one session cookie
server.use(http.csrf())                                  # unsafe methods need a same-origin request
```

## 2. Research

What comparable stacks provide, and what the plan takes from each.

| Feature | Express | FastAPI / Starlette | Go `net/http` | Taken here |
|---|---|---|---|---|
| CORS | `cors` package | `CORSMiddleware` | none built in (`rs/cors`) | A middleware with Starlette's options. Preflights are answered before routing. |
| Static files | `express.static` (`serve-static`) | `StaticFiles` | `http.FileServer` | Go's behaviour: ETag and Last-Modified, 304s, `index.html`, ranges, and no `..` or dotfiles. |
| Compression | `compression` | `GZipMiddleware` | none built in | A gzip middleware with a minimum size, and pre-compressed `.br` and `.gz` static files. |
| Request logs | `morgan` | uvicorn's access log | `slog` with middleware | One structured line per request, with a request ID that other log lines also carry. |
| Metrics | `prom-client` | `prometheus-fastapi-instrumentator` | `prometheus/client_golang` | Built into the runtime: per-route counters and histograms, the Prometheus text format, and a dashboard. |
| Health | `terminus` | user routes | user routes | `/healthz` (liveness) and `/readyz` (readiness, with checks, 503 while shutting down), following Kubernetes probes. |
| Traces | OpenTelemetry SDK | OpenTelemetry SDK | OpenTelemetry SDK | Spans per request and per outgoing call. W3C `traceparent` in and out, sent as OTLP over HTTP with JSON. |
| Body limits | `express.json({limit})` | none (server setting) | `http.MaxBytesReader` | Per route (`max_body=`) and per group. Large bodies are read only when the handler asks. |
| Forms and uploads | `multer` | `Form`, `UploadFile` | `ParseMultipartForm` | `http.Form[T]` and `http.Upload` parameters, documented in OpenAPI. |
| Timeouts | `connect-timeout` | none | `TimeoutHandler`, `context` | Per route (`timeout=`). The handler's waits are cancelled at the deadline and when the client disconnects. |
| Rate limits | `express-rate-limit` | `slowapi` | `x/time/rate` | Token buckets per client IP or a key function, with `RateLimit` and `Retry-After` headers. |
| Config | `dotenv` | `pydantic-settings` | `os.Getenv`, `envconfig` | pydantic-settings' model: a typed struct filled from `.env` and the environment. |
| Streaming, SSE | `res.write` | `StreamingResponse`, `sse-starlette` | `Flusher` | `http.stream` and `http.sse`, with heartbeats and `Last-Event-ID`. |
| Templates | EJS, Pug | Jinja2 | `html/template` | A Jinja-like subset that escapes everything by default, as `html/template` and Jinja2 with autoescape do. |
| Dates | `Date`, `date-fns` | `datetime` | `time` | Go's model: an instant, a duration, and time zones read from the system's database. |
| UUIDs | `crypto.randomUUID` | `uuid` | `google/uuid` | Versions 4 and 7 (RFC 9562). |
| Migrations | `knex`, `prisma` | Alembic | `golang-migrate` | `golang-migrate`'s model: numbered SQL files applied once each, forward only. |
| Jobs | `node-cron`, BullMQ | Celery, APScheduler | goroutines and tickers | In-process schedules and queues first. Durable Postgres-backed jobs come later. |
| CSRF | `csurf` (deprecated) | none | `CrossOriginProtection` (Go 1.25) | Go 1.25's approach: refuse cross-origin unsafe requests by `Sec-Fetch-Site` and `Origin`, with tokens for old browsers. |
| Sign-in | Passport strategies | Authlib | `x/oauth2`, `go-oidc` | OIDC discovery for any compliant provider, presets for common ones, and GitHub's OAuth 2. |

Standards this plan follows (section 8 has the links):
- The Fetch standard's CORS protocol.
- RFC 9110 (conditional requests, ranges, content coding) and RFC 9111 (caching).
- RFC 1952 (gzip) and RFC 7932 (Brotli).
- RFC 3339 and ISO 8601 (dates); RFC 8536 (TZif time zone files); RFC 9562 (UUIDs).
- RFC 7578 (`multipart/form-data`); RFC 7617 (Basic authentication).
- The Prometheus text exposition format 0.0.4.
- OTLP/HTTP with JSON encoding, and W3C Trace Context.
- The HTML standard's Server-Sent Events.
- The IETF draft on `RateLimit` header fields.
- OpenID Connect Discovery 1.0.

## 3. What exists

**The runtime** (`crates/lang_rt/src/http.rs`):
- Each connection runs on a fiber.
- A hand-written parser reads up to 64 KiB of headers and bodies up to `MAX_BODY` (8 MiB, one global `AtomicI64`). It handles chunked bodies, keep-alive, pipelining and `HEAD`.
- An idle connection is closed after 60 s, and a request that takes more than 30 s to arrive gets a 408.
- SIGTERM drains requests in progress, for up to 10 s.
- `serve_connection` loops over these steps: `read_request`, then the program's entry inside `catch_unwind` (a 500 if the handler panicked or didn't respond), then a flush.
- `lrt_http_respond(handle, status, content_type, headers, body)` appends to a per-connection buffer through `write_response`, which adds `Date`, `Server`, `Content-Length` and `Connection`.
- The body is a `str`; the program never builds the status line.
- Per request, the runtime makes 2 `clock_gettime` calls (bench/http/counts.json), both on the coarse clock in `poll.rs`.

**Logging:** `lrt_log_write` formats `2026-11-20T09:30:00.123Z INFO  message` with the runtime's `civil()` date conversion, and writes one line straight to stderr. It takes a level and a message, and no fields.

**The client** (`crates/lang_rt/src/client.rs`): one request per connection, with a fixed 30 s `TIMEOUT`.

**The standard library:**
- `http`:
  - routes, groups, middleware (`def(Request, Handler) -> Result[Response]`) and `on_error`;
  - cookies, `set_local` and `local`;
  - `test_request`, `docs` and the OpenAPI document;
  - WebSockets (which already write `bytes`), the client, and `listen_tls`.
  - `Response.body` is a `str`.
- `log`: `debug`, `info`, `warn`, `error`, `set_level`.
- `time`: `sleep`, `monotonic`, `now` and `since`, all as float seconds.
- `os`: `getenv`, `args`, `exit`, `cwd`. There is no `setenv`.
- `io`: `read_file` (text), `write_file`, `exists`, `list_dir`, `open` (`with` blocks).
- `crypto`: random bytes and tokens, SHA-256, HMAC, constant-time `equal`, base64, and `verify_rs256` on `ring`.
- `auth`: Google sign-in, signed session cookies, and `@auth.required`.
- `postgres`: a hand-written client protocol with `query_as`, and pooled connections.
- `json`, `re`.

**What is missing:** everything in this plan.

**Dependencies** (Cargo.lock): `ring`, `rustls`, `webpki-roots`, `regex-lite`, `getrandom`, `libc`, `once_cell`, `subtle`, `zeroize`, `thiserror`, `anyhow`, `clap`. There is no `flate2`, `miniz_oxide`, `brotli`, `chrono`, `uuid` or `rusqlite`. Agents build with `--offline`, so any new crate needs the owner to approve it and fetch it once (section 5).

## 4. Design

### 4.1 Foundations: bytes bodies, the client's address, request context, structured logs

These are small changes that later sections rely on.

**Bytes bodies:**
- `Response` gains a private `bytes` body. `http.bytes(data, content_type="application/octet-stream", status=200)` builds one.
- `serve` passes it to a new `lrt_http_respond_bytes`.
- `response.body` stays `str` for text. Static files, compression and uploads use bytes.

**The client's address:**
- `req.remote_addr() -> str` is the socket's peer (the runtime has it in `Connection.peer`).
- `req.client_ip() -> str` follows `X-Forwarded-For` (rightmost untrusted address) only when the peer is in `server.trusted_proxies: list[str]`, which holds addresses or CIDR ranges and is empty by default. Otherwise it is the peer.
- Rate limits and logs use `client_ip`.

**Request context:**
- A per-task slot in the runtime (`fiber.rs`) holds the current request's ID and trace context.
- `http.request_log()` sets it, and `log` reads it, so every log line written while a request is handled carries its request ID without passing it around.
- Spawned tasks inherit it.

**Structured logs:**
- `log.info(message, fields={"user": id})`. Each level function gains `fields: dict[str, str] = {}`.
- `log.set_format("text" | "json")`. The default is `text`, or the `LOG_FORMAT` variable.
- Text keeps today's line and appends ` key=value` pairs, quoted when they contain spaces.
- JSON writes `{"time":..., "level":..., "msg":..., "request_id":..., ...fields}` on one line.
- `log.with_fields(fields) -> Logger` gives a logger that adds fields to every line.

**`http.request_log()`:**
- It reads `X-Request-Id` if the client sent a valid one (at most 128 characters from `[A-Za-z0-9._-]`), otherwise it makes one (`uuid.v7()` once 4.5 is built, a random token before that).
- It sets `X-Request-Id` on the response.
- After the handler, it writes one `info` line: method, path without the query string, route pattern, status, duration in ms, bytes out, `client_ip` and request ID.
- `request_log(skip=["/healthz", "/readyz", "/metrics"])` leaves out noisy paths.

**Buffered log output:**
- Today each line is one `write` to stderr.
- With an access log that is a system call per request. `lrt_log_write` gets a per-thread buffer that is flushed when it holds 16 KiB, every 100 ms (by the poller's timer), at `warn` and above, and at exit.

### 4.2 CORS

```python
server.use(http.cors(
    origins=["https://app.example.com"],   # or ["*"]; or a function def(str) -> bool
    methods=["GET", "POST", "PUT", "PATCH", "DELETE"],
    headers=["Content-Type", "Authorization"],
    expose=["X-Request-Id"],
    credentials=False,
    max_age=600,
))
```

- **Preflight:** an `OPTIONS` request with `Origin` and `Access-Control-Request-Method` gets a 204 from the middleware. The router never sees it.
  - Middleware runs around `dispatch`, so this needs no change to routing.
  - The 204 carries `Access-Control-Allow-Origin`, `-Methods`, `-Headers` (the requested ones that are allowed), `-Max-Age`, `-Credentials` when set, and `Vary: Origin, Access-Control-Request-Method, Access-Control-Request-Headers`.
- **Actual requests:** these add `Access-Control-Allow-Origin` (the request's origin when it is allowed, or `*`), `Vary: Origin`, `-Credentials` and `-Expose-Headers`.
- **Disallowed origins:** they get no CORS headers. The browser blocks the response, and the server still answers, as the standard expects.
- **Errors at setup:** `credentials=True` with `origins=["*"]` panics at setup, because browsers reject that combination.
- **Groups:** `group.use(http.cors(...))` scopes CORS to an API prefix.
- **OpenAPI:** nothing changes. The documentation viewer's "Try it out" is same-origin.

### 4.3 Static files

```python
server.static("/assets", "public", max_age=3600, immutable=False, index="index.html", dotfiles=False)
server.static("/", "dist", fallback="index.html")   # a single-page app: unknown paths get index.html
```

- **Registration:** `GET` and `HEAD` routes on `/assets/{path...}`.
  - The handler calls `lrt_http_static(handle, root, path, options)`, which serves the file from Rust: no `str` round trip, and binary-safe.
  - `HEAD` sends the headers only.
- **Path safety**, checked in this order:
  1. Percent-decode once. A decoded `%2F` or `%5C` is refused.
  2. Refuse any segment that is `..`, `.` (it is collapsed instead) or empty in the middle, and any NUL or backslash.
  3. Refuse dotfiles (segments starting with `.`) unless `dotfiles=True`.
  4. Join with the root.
  5. Open the file and `realpath` it. If it is not under the root's `realpath` (symlinks pointing out), answer 404.
  - Refusals are 404, not 403, so they don't reveal what exists.
- **Content types:** a built-in table of about 60 extensions (html, css, js, mjs, json, map, svg, png, jpg, jpeg, gif, webp, avif, ico, woff, woff2, ttf, otf, txt, md, xml, pdf, wasm, mp4, webm, mp3, ogg, wav, csv, zip, gz, ...).
  - Text types get `; charset=utf-8`; anything unknown is `application/octet-stream`.
  - Every response has `X-Content-Type-Options: nosniff`.
- **Validators:**
  - `ETag: W/"<size hex>-<mtime ns hex>"`, from `fstat`, so no hashing.
  - `Last-Modified` as an HTTP date (the runtime already formats `Date`).
  - `If-None-Match` (weak comparison, lists, `*`) takes precedence over `If-Modified-Since`, as RFC 9110 section 13.2.2 says. A match gets a 304 with the validators and `Cache-Control`, and no body.
- **Ranges:** a single `bytes=a-b`, `a-` or `-n` range gives a 206 with `Content-Range`. An unsatisfiable range gives a 416. Multiple ranges get the whole file with a 200 (allowed). `If-Range` is honoured. This is enough for video seeking and resumed downloads.
- **Caching:** `Cache-Control: public, max-age=<max_age>`, plus `, immutable` when set (for hashed file names).
- **Pre-compressed files:**
  - If `Accept-Encoding` allows `br` (or `gzip`) and `file.br` (or `file.gz`) exists next to the file, that file is sent with `Content-Encoding` and `Vary: Accept-Encoding`.
  - This works with no compression crate, and build tools already produce these files.
- **Reading:**
  - Files up to 1 MiB are read whole into the connection's out buffer.
  - Larger files are sent in 256 KiB chunks, with the fiber parking on the socket between chunks, so a 1 GiB download never holds 1 GiB.
  - `sendfile(2)` on plain sockets is a later optimisation. It is not possible under TLS.
- **Blocking reads:** file reads block the worker thread, as `io.read_file` does today. This is acceptable for local disks, and section 7 notes it.

### 4.4 Compression

```python
server.use(http.compress(min_size=1024, level=6, types=["text/*", "application/json", "application/javascript", "image/svg+xml"]))
```

- **When it compresses:** a response is compressed when all of these hold:
  - the request's `Accept-Encoding` allows `gzip` (q-values parsed; `identity;q=0` honoured);
  - the body is at least `min_size`;
  - its type matches;
  - it has no `Content-Encoding`;
  - it is not a stream (4.11), a `206` or a `304`.
- **Headers:** compressed responses get `Vary: Accept-Encoding`. A strong `ETag` becomes weak.
- **Encoder:** gzip through the encoder chosen in section 5 (decision 1), behind `lrt_gzip(bytes, level) -> bytes`.
- **Brotli:** dynamic Brotli is not planned. Its encoder is large, and pre-compressed `.br` files (4.3) cover the assets where it matters.
- **Without an encoder:** if the owner declines a crate, `http.compress` is not built, and the milestone ships only the pre-compressed static files.

### 4.5 Dates, times, durations and UUIDs

A new `datetime` module, written by hand on the runtime's `civil()` conversion and the system clock. It needs no crate.

```python
import datetime

now = datetime.now()                                 # DateTime, UTC
t = datetime.parse("2026-11-20T09:30:00+01:00")?     # RFC 3339 / ISO 8601
t.format_iso()                                       # "2026-11-20T08:30:00Z"
t.in_zone("Europe/London")?.format("%Y-%m-%d %H:%M %Z")
t + datetime.hours(2); t - other                     # Duration
datetime.date(2026, 11, 20).weekday()                # Date, without a time
datetime.minutes(5).seconds()                        # 300.0
```

- **Types:**
  - `DateTime` is an instant: integer nanoseconds since the Unix epoch, plus the offset in minutes it is shown with.
  - `Date` is year, month and day.
  - `Duration` is integer nanoseconds.
  - `Zone` is a loaded time zone.
  - All of them are immutable values, so they can be shared.
- **Parsing:** `parse` accepts RFC 3339 and these ISO 8601 forms: `T` or a space between date and time, fractional seconds up to 9 digits, `Z` or `±hh:mm`. Anything else is an error that says which character was unexpected. `parse_date` accepts `YYYY-MM-DD`.
- **Formatting:**
  - `format_iso()` is RFC 3339 with `Z` for UTC.
  - `format(pattern)` takes a documented strftime subset: `%Y %m %d %H %M %S %f %z %Z %a %A %b %B %j %%`.
  - `http_date()` gives the IMF-fixdate.
- **Time zones:**
  - `datetime.zone(name)` reads TZif files (RFC 8536, versions 1 to 3) from `TZDIR` or `/usr/share/zoneinfo`, and caches them.
  - It handles the version-2 footer's POSIX TZ string for dates past the table.
  - `local()` uses `TZ` or `/etc/localtime`.
  - Windows has no zoneinfo; see section 5, decision 2.
- **Durations:**
  - Constructors: `seconds`, `millis`, `minutes`, `hours`, `days`.
  - `parse_duration("1h30m")` uses Go's syntax.
  - Durations are what `time.sleep`, timeouts (4.10) and jobs (4.15) take from now on. The float forms stay for compatibility.
- **JSON and OpenAPI:**
  - `DateTime` implements `ToJson` and `FromJson` as RFC 3339 strings, `Date` as `YYYY-MM-DD`, and `Duration` as Go-syntax strings.
  - The JSON derive must accept them as field types. Check that it accepts any type with these impls, and teach it if not.
  - `openapi.rs` maps them to `{"type":"string","format":"date-time"}` and `"format":"date"`.
  - `postgres.query_as` decodes `timestamptz`, `timestamp` and `date` columns into them.
- **Tests:** `datetime.set_clock_for_tests(fixed)` makes `now()` return a fixed instant in tests.

A new `uuid` module:
- `uuid.v4()` is random, from `crypto.random_bytes`.
- `uuid.v7()` is a Unix-millisecond timestamp plus random bits, monotonic within a process (RFC 9562, section 6.2, method 1). Use it for database keys and request IDs.
- `uuid.parse(text)` accepts the hyphenated form, either case. `str(u)` gives lowercase.
- `Uuid` implements `ToJson`/`FromJson` and maps to `format: uuid`.
- It is also usable as a path parameter: `def get(self, id: uuid.Uuid)` gives a 400 on a malformed ID, as `int` parameters do today.

### 4.6 Configuration and `.env`

A new `config` module:

```python
config.load_env(path=".env", override=False)     # sets variables that are not already set
settings = config.load[Settings](prefix="", env_file=".env")?
os.setenv("NAME", "value")                       # new in `os`
```

- **The `.env` syntax**, documented exactly:
  - `KEY=value` per line, with an optional `export ` prefix.
  - `#` starts a comment, at the start of a line or after whitespace in an unquoted value.
  - Single-quoted values are literal. Double-quoted values allow `\n`, `\"` and `\\`, and may span lines.
  - Unquoted values are trimmed.
  - There is no variable interpolation (decision 7).
  - A malformed line is an error that gives its line number. It is never silently skipped.
  - A missing file is not an error for `load`, but is for `load_env` called with an explicit path.
- **Precedence:** the real environment wins over `.env`, so deployments override files.
- **`load[T: FromJson]`:**
  - It builds an object from the variables. Field `database_url` is read from `PREFIX + DATABASE_URL`.
  - It decodes the object with a lenient mode of the JSON decoder: a string where an `int`, `float` or `bool` is expected is parsed (`true/false/1/0/yes/no` for booleans), and a `list[str]` field is read comma-separated.
  - Fields with defaults are optional; a missing required field is an error.
  - **One error lists every problem**, such as `config: DATABASE_URL is required; PORT must be an integer, got "80a"`, so a deployment is fixed in one go.
  - This needs a lenient flag in `crates/lang_rt/src/json.rs`'s decoder. Read how `FromJson` is generated before choosing where the flag goes; the alternative is a new derive (decision 8).
- **Secrets:**
  - `config.Secret` is a `str` wrapper whose `str()` and log output are `***`. `secret.reveal()` returns the value.
  - Use it as a field type for passwords and keys, so `log.info(f"{settings}")` never leaks them.

### 4.7 Metrics, health and readiness

**Metrics live in the runtime**, as atomics, so recording costs no allocation and no lock.

- **Turning it on:**
  - `server.metrics(...)` sets `METRICS_ON`.
  - Until then the request path makes no extra clock reads and touches no counters, so `bench/http/counts.py --check` is unchanged (4.13).
- **Labels:**
  - Each route gets an index when `compose` runs. `dispatch` tells the runtime which route matched with `lrt_http_set_route(handle, index)`, one store into the request struct.
  - Requests that match no route are labelled `route="unmatched"`. **Labels are never the raw path**, so the number of series stays bounded.
- **What is recorded:**
  - **Per route:**
    - `http_requests_total{method,route,status}`;
    - `http_request_duration_seconds` histogram;
    - `http_response_size_bytes` sum.
    - `status` is the exact code, limited to codes actually seen, which is at most a few dozen.
  - **Per server:**
    - `http_requests_in_flight`, `http_connections_open`, `http_connections_total`;
    - `http_request_errors_total{kind="panic"|"timeout"|"parse"|"too_large"}`.
  - **Per process:**
    - `process_start_time_seconds`, `process_resident_memory_bytes` (from `/proc/self/statm`);
    - `runtime_heap_bytes` and `runtime_gc_collections_total` (from the collector);
    - `runtime_tasks`, `runtime_worker_threads`;
    - `build_info{version}`.
- **The histogram:** 16 buckets, finer than Prometheus' defaults because handlers here take microseconds: 0.1 ms, 0.25, 0.5, 1, 2.5, 5, 10, 25, 50, 100, 250, 500 ms, 1, 2.5, 5, 10 s, and `+Inf`.
- **Counter storage:**
  - Counters are sharded per worker thread: one array of `AtomicU64` per thread, padded to a cache line, merged when scraped. Threads never write to the same line.
  - Routes beyond the first 1024 share one series, with a warning at startup.
- **Timing:**
  - One `CLOCK_MONOTONIC` read when the request has been parsed, and one when its response is written.
  - The coarse clock is too coarse at 1 to 4 ms.
  - Under TLS, the time includes encryption, which is what the client waits for.

**Application metrics:**

```python
jobs_done = metrics.counter("jobs_done_total", "Jobs finished", labels=["queue"])
jobs_done.inc({"queue": "email"})
queue_depth = metrics.gauge("queue_depth", "Jobs waiting")
db_time = metrics.histogram("db_query_seconds", "Query time")   # the default buckets above
with db_time.time():
    rows = db.query(...)?
```

- Each one is a registry ID (an `int`) into the runtime's registry, so it can be shared.
- The number of label values is capped at 1000 per metric. Beyond that, a warning is logged once and the values are folded into `other`.

**`server.metrics(path="/metrics", dashboard=True, user=None, password=None, public=False)`:**
- `GET /metrics` serves the Prometheus text format, version 0.0.4, with `# HELP` and `# TYPE` lines.
- The dashboard (4.8) is at `path + "/dashboard"`.
- Credentials come from the arguments, else from `METRICS_USER` and `METRICS_PASSWORD` (or `METRICS_PASSWORD_HASH`).
- With none of these, and without `public=True`, it **panics at setup**: "server.metrics needs a username and password: set METRICS_USER and METRICS_PASSWORD in the environment or in .env (read by config.load or config.load_env), or pass public=True to serve the metrics without them."
- These routes are left out of the metrics themselves and out of the OpenAPI document.

**Health and readiness:**

```python
server.health(live="/healthz", ready="/readyz", checks={"database": lambda: db.ping()})
```

- `/healthz` always answers `200 ok` while the process serves.
- `/readyz` runs the checks, each with a 2 s limit, concurrently. It answers 200 with `{"database":"ok"}` if all pass, and 503 listing the failures if not.
- **On SIGTERM**, `/readyz` answers 503 at once, and the server keeps serving for `server.drain_delay` (default 0 s; 5 s is typical on Kubernetes) before it stops accepting. Load balancers stop sending traffic before connections close.
- Both are left out of the access log by default and out of the OpenAPI document.

### 4.8 The metrics dashboard

One HTML page, one stylesheet and one script, embedded in the runtime like the documentation viewer:
- the files are `crates/lang_rt/assets/dashboard/{index.html, dashboard.css, dashboard.js}`;
- they are reached only through `lrt_dashboard_asset(index)`, so `--gc-sections` drops them from programs that never call `server.metrics`;
- they are served with ETags like the viewer's files;
- there is no CDN, no external fonts, and no chart library. Charts are hand-drawn SVG, kept under about 30 KB of script.

**Authentication: HTTP Basic** (RFC 7617, decision 6). It is simple, Prometheus scrapers support it, and it needs no login page, session or CSRF handling.
- The check runs as middleware on the metrics routes only. A missing or wrong `Authorization` gets a 401 with `WWW-Authenticate: Basic realm="metrics", charset="UTF-8"`.
- The comparison is constant-time:
  - the username and password are each HMAC-SHA256'd with a random per-process key;
  - the digests are compared with `crypto.equal`;
  - so neither the length nor the content leaks through timing.
- **`METRICS_PASSWORD_HASH`** is an alternative to a plaintext password in the environment.
  - Its format is `pbkdf2-sha256$<iterations>$<salt b64>$<hash b64>`.
  - `ring::pbkdf2` is already a dependency.
  - `viper hash-password` prints one, prompting without echo. The default is 600,000 iterations, OWASP's 2023 figure for PBKDF2-SHA256.
  - A verified hash is cached for the process (keyed by the HMAC of the presented password), so the dashboard's polling does not pay for PBKDF2 each time.
- **Failed attempts:** after 10 failures from one `client_ip` in a minute, that address gets 429s for a minute, with a `warn` log line. This uses the rate limiter of 4.12 once it exists; before then, it uses a small fixed table in the metrics module.
- **Plain HTTP:** if the server is not TLS and the request did not come through a trusted proxy with `X-Forwarded-Proto: https`, a warning is logged once that Basic credentials are crossing plain HTTP. Loopback addresses are exempt.
- **Headers:** responses carry these:
  - `Cache-Control: no-store`;
  - `X-Frame-Options: DENY`;
  - `Content-Security-Policy: default-src 'self'; style-src 'self'; script-src 'self'; img-src 'self' data:`;
  - `Referrer-Policy: no-referrer`.

**The runtime's history:**
- A background thread, started by `server.metrics`, takes a snapshot every 5 s into a ring of 720 samples (1 hour).
- A sample holds:
  - requests by status class;
  - latency percentiles;
  - in-flight requests, connections, heap, resident memory and tasks;
  - per-route deltas.
- The ring is about 2 MB with 100 routes.

**Recent events:**
- The runtime keeps the last 50 responses with status ≥ 500, and the 20 slowest of the last 5 minutes.
- Each records:
  - the time, method and route;
  - the path **without its query string**, because queries often carry tokens;
  - the status, duration and request ID.
- Recording happens only when metrics are on. It is a compare against the current minimum, plus a short lock taken only when a request qualifies.

**Data:** `GET <path>/dashboard/data.json?since=<sample>` returns the samples after `since` and the current totals, so a polling page only downloads what is new.

**The page:**
- **Header tiles:**
  - uptime and version;
  - requests per second over the last minute;
  - error rate (5xx) and p50, p95 and p99 latency;
  - in-flight requests and open connections.
- **Charts** for the last 5 minutes, 15 minutes or 1 hour (a selector):
  - requests per second stacked by status class (2xx, 3xx, 4xx, 5xx);
  - latency p50, p90 and p99;
  - in-flight requests and connections;
  - heap, resident memory and tasks.
- **Routes table**, sortable and filterable: method, pattern, requests, requests per second, 4xx %, 5xx %, p50, p95, p99.
- **Recent server errors** and **slowest requests**, each with its request ID, to find in the logs.
- **Application metrics:** each counter and gauge as a number with a sparkline.
- It refreshes every 5 s, has a pause button, follows light and dark mode, and works at phone width.
- With streaming (4.11), the page switches to Server-Sent Events. Polling stays as the fallback.

**Percentiles** are estimated from the histogram by linear interpolation within a bucket, as Prometheus' `histogram_quantile` does. The page says "approximately".

### 4.9 Traces

A new `trace` module that speaks OTLP over HTTP with JSON encoding. It needs no protobuf, no gRPC and no crate.

- **Turning it on:** `trace.init(service="photos", endpoint=os.getenv("OTEL_EXPORTER_OTLP_ENDPOINT"))`, or `http.request_log(trace=True)`. With no endpoint it is off.
- **The trace context:**
  - Incoming `traceparent` and `tracestate` are parsed and continued.
  - Without them, a new trace starts, with `OTEL_TRACES_SAMPLER_ARG` as a ratio (default 1.0).
  - The trace ID is stored in the request context (4.1), so logs carry `trace_id` and `span_id`.
- **Spans:**
  - one server span per request, named `METHOD route`, with the HTTP semantic-convention attributes (`http.request.method`, `http.route`, `http.response.status_code`, `url.path`, `client.address`);
  - client spans for `http.fetch`, which also sends `traceparent`;
  - `postgres` query spans with `db.system` and the statement's first word (never the parameters).
- **Spans in the program's own code:** `with trace.span("resize image", {"width": str(w)}):`, using the `with` statement that `io.open` already uses.
- **Export:**
  - Spans are batched by a background task: every 5 s or 512 spans.
  - They are POSTed to `<endpoint>/v1/traces` as JSON.
  - At most 2048 are queued; beyond that, spans are dropped and counted in `trace_spans_dropped_total`.
  - A final flush runs at shutdown.
- **Logs and metrics over OTLP** are left out: logs go to stdout and stderr, and metrics to Prometheus, which collectors already scrape.

### 4.10 Request size limits, lazy bodies, timeouts and cancellation

**Lazy bodies.** Today `read_request` reads the whole body, up to the global `MAX_BODY`, before any routing. Per-route limits need routing first.
- Bodies up to 64 KiB, which are nearly all API requests, stay eagerly read. The fast path and pipelining are unchanged.
- For a larger `Content-Length`, or a chunked body that grows past 64 KiB, the runtime stops after the headers and marks the body pending.
- `req.text()`, `req.bytes()`, `http.body[T]`, forms and uploads then read it against the **route's** limit: `@http.post(..., max_body=N)`, else the group's `max_body`, else `server.max_body`. Past the limit, the request gets a 413 and the connection closes.
- `Expect: 100-continue` is answered with `100 Continue` only when the handler first reads the body. A refused upload never gets sent.
- If the handler responds without reading a pending body, up to 1 MiB of it is drained to keep the connection. Beyond that, the connection closes after the response.
- The global `MAX_BODY` becomes the default for `server.max_body`, and stays a hard cap on what is read eagerly.
- **Other guards:** at most 100 headers (431), and a request target of at most 8 KiB (414). Both can be changed on `Server`.

**Per-call client timeouts:**
- `http.fetch(..., timeout=datetime.seconds(30), connect_timeout=datetime.seconds(5))`.
- The runtime's fixed `TIMEOUT` becomes the default, and the timeout error says which limit expired.

**Per-route timeouts:**
- `@http.get(..., timeout=10.0)`, `group.timeout` and `server.timeout`. The default is none, as today.
- At the deadline, the request's task is **cancelled** and the client gets a 503 (decision 9) with `Retry-After: 1`. The handler's eventual response, if any, is discarded.

**Cancellation:**
- The cancel flag and deadline sit in the per-task context (4.1).
- Every park point checks them: socket reads and writes, `sleep`, channel receives, mutex waits, client calls and `postgres` queries.
  - A cancelled wait returns the operation's error, with the kind `Cancelled`. `sleep` returns early.
  - Handlers written with `?` unwind on their own.
  - Code that never waits (a CPU loop) is not interrupted. The documentation says so, and `http.check_cancelled()?` is there for long loops.
- **Client disconnects:** while a handler runs, the connection's socket is registered for `EPOLLRDHUP`. A hang-up cancels the handler, and is counted as `http_request_errors_total{kind="client_closed"}`. Pipelined data that arrives is not a hang-up; only `RDHUP` or `HUP` is.
- **Tasks spawned by the handler** inherit the deadline, but not cancellation by disconnect, unless spawned with `spawn_scoped` (a later addition). This is documented.
- `req.deadline() -> datetime.DateTime?` and `req.cancelled() -> bool` are there for code that wants to check.

### 4.11 Streaming responses and Server-Sent Events

```python
return http.stream(req, lambda out: export_csv(db, out), content_type="text/csv")

def export_csv(db: Db, out: http.Stream) -> Result[None]:
    for row in db.query("select ...")?:
        out.write(row.csv_line())?        # Err(Closed) when the client goes away
    return Ok(None)
```

- **Streams:**
  - `http.stream` responds with `Transfer-Encoding: chunked` (HTTP/1.1), or by closing the connection (HTTP/1.0).
  - The callback runs in the connection's task, as WebSocket handlers do, using the existing `upgraded`-style hand-over in `serve_connection`.
  - `out.write(str | bytes)` buffers and sends at 16 KiB. `out.flush()` sends now.
  - The response's status and headers are sent with the first flush.
- **SSE:** `http.sse(req, handler)` is a stream with `Content-Type: text/event-stream`, `Cache-Control: no-store` and `X-Accel-Buffering: no`.
  - `events.send(data, event="", id="", retry=0)` splits `data` on newlines into `data:` lines and flushes.
  - A comment line (`: ping`) is sent every 15 s (`heartbeat=`) so proxies keep the connection.
  - `events.last_id()` reads the `Last-Event-ID` header for resuming.
  - `events.closed()` is true once the client has gone.
- **Exclusions:** both are left out of `http.compress` and of route timeouts (they would end every stream). They are counted in metrics when they end.
- **Testing:** `server.handle(test_request(...))` collects the stream into the response body, so tests can check it.
- **OpenAPI:** SSE routes document `text/event-stream`.

### 4.12 Rate limiting

```python
server.use(http.rate_limit(per_second=20.0, burst=40))                            # per client IP
api.use(http.rate_limit(per_second=5.0, burst=10, key=lambda req: auth.user(req).map(lambda u: u.id).unwrap_or(req.client_ip())))
```

- **The algorithm:** a token bucket per key. Each key stores its tokens and the time of the last refill, so a request costs one map lookup and some arithmetic.
- **Storage:**
  - Keys are spread over 64 shards, each a `Mutex` of `dict[str, Bucket]`, picked by hash.
  - Idle keys are dropped by a sweep every minute.
  - The total is capped at 100,000 keys. Past that, the oldest are evicted, so an attacker cycling addresses cannot exhaust memory.
- **Responses:**
  - Allowed requests get `RateLimit-Policy` and `RateLimit` headers (per the IETF draft, decision 12).
  - Refused ones get a 429 with `Retry-After` and a JSON or text body like other errors (`on_error` applies).
- **Scope:** limits are per process. Several instances need a shared store, and a Redis backend (section 6, Milestone 25) can come later. This is documented.
- **Per-route options:** `@http.post(..., rate_limit="5/minute")` is a short form that the compiler turns into the middleware on that route.

### 4.13 Forms and uploads

- **`req.form() -> Result[Form, HttpError]`:**
  - It works for `application/x-www-form-urlencoded` and `multipart/form-data`.
  - `form.get(name) -> str?`, `form.all(name) -> list[str]`, `form.files(name) -> list[Upload]`.
- **`Upload`:**
  - Fields: `filename` (sanitised: the base name only, with no path separators and no control characters), `content_type`, `size`.
  - Methods: `bytes()`, `text()`, `save(path)`, `open()`.
- **The multipart parser** is in Rust (`crates/lang_rt/src/multipart.rs`), built on the lazy body of 4.10.
  - It follows RFC 7578: a boundary of at most 70 characters, `Content-Disposition` with `name` and `filename` (and `filename*`), and a per-part `Content-Type`.
  - Parts up to 1 MiB stay in memory. Larger ones are written to a temporary file in `server.upload_dir` (default the system temp directory), which is removed when the request ends unless `save` moved it there.
  - Limits: `max_files` (default 10), `max_fields` (1000) and `max_part_headers` (8 KiB), each a 413 or 400 with a clear message.
- **Handler parameters**, bound by `mount` (`check/http.rs`):
  - `form: http.Form[T]` decodes the form's text fields into `T` (a `FromJson` type) with the lenient decoder of 4.6. A missing or bad field is a 400 naming the field.
  - `name: http.Upload` takes the file in the part called `name`. `http.Upload?` makes it optional, and `list[http.Upload]` takes several.
  - They cannot be combined with a JSON body parameter. That is a new compile error, `E0343`.
- **OpenAPI:** such routes document a `multipart/form-data` (or urlencoded) request body. Text fields come from `T`'s schema, and files are `{"type":"string","format":"binary"}`. The viewer's "Try it out" gets a file input.

### 4.14 HTML templates

A new `template` module: a small Jinja-like language, parsed at startup and rendered with **auto-escaping**.

```python
pages = template.load_dir("templates", reload=os.getenv("DEV").is_some())?
return pages.render("home.html", {"user": user, "photos": photos})     # a ToJson value as context
```

- **Syntax:**
  - Output: `{{ expr }}`.
  - Statements: `{% if %}`, `{% elif %}`, `{% else %}`, `{% for x in xs %}`/`{% empty %}`, `{% set %}`, `{% include "x.html" %}`, `{% extends "base.html" %}` with `{% block name %}`.
  - Comments: `{# #}`.
- **Expressions:**
  - field access and indexing;
  - literals;
  - `==`, `!=`, `<`, `>`, `and`, `or`, `not`, `in`;
  - the filters `upper`, `lower`, `title`, `trim`, `length`, `default(x)`, `join(s)`, `date(fmt)`, `json`, `url`, `raw`.
- **The context:** anything `ToJson`. The template sees the JSON tree, which keeps the language's static types out of the template engine.
  - A missing field is an error at render time that names the template, line and expression.
  - Rendering is never silently blank (decision 10).
- **Escaping:**
  - Every `{{ }}` is HTML-escaped (`& < > " '`) unless it uses `| raw`.
  - Inside `<script>` and `<style>` blocks and in `on*=` and `style=` attributes, `{{ }}` without `| json` is a **load-time error**, because HTML escaping is not enough there.
  - `href` and `src` values that start with a scheme other than `http`, `https` or `mailto` are replaced with `#unsafe`.
  - The tracking of where a `{{ }}` sits is a light tokenizer over the template text at load time, not a full HTML parser.
- **Reload:** `reload=True` rereads changed files on each render (by modification time), for development.
- **Helpers:** `http.render(pages, name, ctx, status=200)` returns an HTML `Response`. `csrf_field()` (4.17) is available in templates.

### 4.15 SQLite and migrations

**`sqlite`** has the same shape as `postgres`: `open(path)`, `query`, `query_as[T]`, `execute`, `transaction`, and parameters as `?`.
- It is built on the system's `libsqlite3` through hand-written FFI in `crates/lang_rt/src/sqlite.rs` (decision 4). It is linked only when a program imports `sqlite`; the driver adds `-lsqlite3`.
- SQLite calls block. Each database gets a dedicated OS thread that owns the connection. Tasks send it work over a channel and park until the reply arrives, so worker threads never block on disk.
- It opens with `journal_mode=WAL`, `busy_timeout=5000` and `foreign_keys=ON`.
- Types:
  - `INTEGER` → `int`, `REAL` → `float`, `TEXT` → `str`, `BLOB` → `bytes`, `NULL` → `None`;
  - `DateTime` and `Uuid` are stored as text.

**`migrate`:**
- `migrate.apply(db, "migrations")` works with `postgres` and `sqlite`.
- **Files** are `NNNN_name.sql`, applied in numeric order, each in a transaction. A file can opt out with a first line of `-- migrate: no-transaction`, for `CREATE INDEX CONCURRENTLY`.
- **Bookkeeping:** applied files are recorded in `schema_migrations(version, name, checksum, applied_at)`. A changed checksum on an applied file is an error: "migration 0003_add_tags.sql was edited after it ran".
- **Locking:** on Postgres, `pg_advisory_lock` keeps two instances starting together from both migrating. On SQLite, the write lock does this.
- **Forward only** (decision 11). `migrate.status(db, dir)` lists what is pending.
- **The CLI:** `viper migrate new <name>` creates the next numbered file, and `viper migrate status` reports.
- **Embedding:** migrations are files beside the program. Embedding them in the binary needs a compile-time `embed` feature, which is out of scope (section 7).

### 4.16 Background jobs and scheduling

A new `jobs` module, all in-process:

```python
jobs.every(datetime.minutes(5), refresh_feed)                       # first run after 5 minutes
jobs.cron("0 3 * * *", prune_old, zone="Europe/London")             # 5-field cron, DST-aware
mail = jobs.Queue[Email](workers=4, retries=3, backoff=datetime.seconds(10))
mail.submit(Email(...))
```

- **Schedules:**
  - Each schedule runs in its own task.
  - A run that is still going when the next is due is skipped, and the skip is logged (no overlap).
  - Each run has the request-like context: a job ID in logs and a span when tracing is on.
- **Cron:** the standard 5 fields with `*`, lists, ranges, steps, and the `@hourly`, `@daily` and `@weekly` names.
  - Times are evaluated in the given zone.
  - A local time that doesn't exist (the spring-forward gap) runs at the next valid instant. A repeated one (fall-back) runs once.
- **`Queue[T]`:**
  - It is bounded (`capacity=1000`); `submit` returns `Err(Full)` when the queue is full.
  - It runs `workers` concurrent tasks.
  - Failures are retried with exponential backoff and jitter, then logged as failed and counted.
  - Metrics: `jobs_queued`, `jobs_running`, `jobs_failed_total` per queue.
- **Shutdown:** SIGTERM stops schedules, lets running jobs finish within the drain time, and logs how many queued jobs were dropped. In-process queues are not durable, and the documentation says so.
- **Later:** durable queues in Postgres (`FOR UPDATE SKIP LOCKED`) are a later milestone, with the same `Queue` API and `jobs.Queue[T](store=postgres_store(db))`.

### 4.17 CSRF protection

`server.use(http.csrf(trusted_origins=[]))`:
- **What it refuses:** for unsafe methods (anything but `GET`, `HEAD` and `OPTIONS`):
  - a request whose `Sec-Fetch-Site` is `cross-site`;
  - or, when `Sec-Fetch-Site` is absent, a request whose `Origin` is present and is neither the request's own origin nor in `trusted_origins`.
  - Refusals get a 403 with a clear message.
- **What it lets through:**
  - requests with neither header (non-browser clients, which ambient cookies cannot be forged for);
  - requests with an `Authorization` header (not ambient).
- This is the approach of Go 1.25's `CrossOriginProtection`, and it needs no token plumbing for modern browsers (decision 13).
- **Tokens** for forms that must also work in old browsers: `http.csrf(tokens=True)`.
  - It sets a signed `csrf` cookie and checks a `csrf_token` form field or an `X-CSRF-Token` header against it, with the double-submit cookie signed by `SESSION_SECRET`.
  - `http.csrf_token(req)` and the templates' `{{ csrf_field() }}` give the value.
- **With `auth`:** `auth.install` adds `http.csrf()` to the server unless `csrf=False` is given. Cookie sessions without CSRF protection are the common mistake.

### 4.18 Generic OpenID Connect, presets, and GitHub

`auth` already has everything except discovery and other algorithms: the flow, PKCE, state and nonce, the key cache, RS256 and sessions.

- **`auth.oidc(issuer, client_id, client_secret, scopes=["openid","email","profile"], name="oidc")`:**
  - It fetches `<issuer>/.well-known/openid-configuration` at startup, with one retry, and fails startup if the fetch fails.
  - It checks that the document's `issuer` equals `issuer` exactly.
  - It takes the authorize, token and JWKS endpoints from the document.
  - `auth.oidc_for_tests(endpoints=...)` skips discovery, like `google_for_tests`.
- **ES256:** `crypto.verify_es256` on `ring`'s ECDSA P-256, for providers that sign with it. RS256 and ES256 are accepted, and only what the discovery document lists.
- **Presets:**
  - `auth.microsoft(tenant="common" | "organizations" | "<id>")`. For multi-tenant use, the issuer has a `{tenantid}` placeholder, and the token's `iss` is checked against the template with its `tid` claim.
  - `auth.okta(domain)`.
  - `auth.auth0(domain)`.
  - `auth.google` itself becomes a preset over the same code, with its endpoints still built in (D102).
- **GitHub** (`auth.github(client_id, client_secret, scopes=["read:user","user:email"])`) is OAuth 2 with no ID token.
  - After the code exchange, the server calls `GET https://api.github.com/user` and `/user/emails` with the access token. GitHub requires a `User-Agent` header, which is the program's name.
  - The user is built from the verified primary email.
  - PKCE is sent; GitHub supports it for OAuth apps.
- **Several providers:**
  - `auth.install_all(server, providers)` gives each its routes under `/auth/<name>/`.
  - One session cookie is shared, and `User.provider` says who signed the user in.
  - `User.id` is `"<provider>:<subject>"`, so IDs never collide between providers.
  - The compiler's `googleSession` scheme name (D104) becomes `session`, with a deprecated alias.

### 4.19 Lower priority

- **Response caching and ETags for dynamic responses:**
  - `http.etag()` middleware hashes the body (FNV-1a, as `lrt_docs_hash` does) and answers `If-None-Match` with a 304.
  - `http.cache(ttl, key=...)` keeps an in-memory cache of `GET` responses, bounded by bytes, with least-recently-used eviction.
  - `Response.cache_control(...)` is a helper.
- **A Redis client:** RESP3 over the existing socket layer, hand-written like `postgres`, with pooling. It is also a shared store for `rate_limit` and `jobs`.
- **A radix-tree router:** only if a benchmark with 200 parameterised routes shows the linear scan in `dispatch` costing more than 5% at the `/users/42` endpoint. Fixed paths already use a map.
- **HTTP/2:** not planned. TLS-terminating proxies (nginx, Caddy, cloud load balancers) speak HTTP/2 to clients and HTTP/1.1 to the app. The documentation says so. A later plan could use the `h2` crate, which would need approval.

### 4.20 The per-request cost

The gates `bench/http/counts.py --check` and `bench/run.py --check` must stay green. These rules keep them so:

1. **Features cost nothing unless used.** Metrics recording, the dashboard, tracing, request IDs, rate limits, CORS and compression are all opt-in. The four benchmark endpoints use none of them, so their counts must not change at all.
2. **The new optional request state** goes into `RequestExtra` or the runtime's `Request` struct, never into new fields on the `str`-holding `Request` value. Docs/13 milestone 1 measured 16 bytes per request for one extra field.
3. **The lazy-body threshold** is chosen so that every benchmark request is read eagerly, as now.
4. **Cost of an observed server:** a new mode, `bench/http/counts.py --observed`, runs the same server with `request_log()` and `metrics()` on, and records `counts_observed.json`. Its budget, compared with the plain counts, is:
   - +2 `clock_gettime` (metrics);
   - +1 allocation and +64 bytes (the request ID);
   - +0 system calls (log lines buffered, 4.1).
   - The rate measured by `bench/run.py` may drop by at most 5% at 256 connections. Measure it once and record the figure in the milestone's Done note.
5. **Never run the benchmarks at the same time as a build or tests** (section 6's rule from docs/11).

### 4.21 Testing, for the language's users

- **Middleware** works with `server.handle(http.test_request(...))` as routes do: CORS preflights, compression (the test response's body is the compressed bytes, with `response.decoded()` to read it), rate limits and CSRF.
- **Uploads:** `http.test_request(..., form={"title": "x"}, files={"image": http.test_file("a.png", data, "image/png")})` builds a multipart body.
- **The clock:** `datetime.set_clock_for_tests` fixes it, and `jobs.run_due_for_tests()` runs due schedules without waiting.
- **Metrics:** `metrics.snapshot_for_tests()` returns the counters as a dict.
- **Templates:** `pages.render` in a test returns the string.

### 4.22 Out of scope

- HTTP/2 and HTTP/3; gRPC.
- An ORM or query builder.
- Durable job queues backed by anything but Postgres.
- Embedding files (templates, migrations, static assets) in the binary. This needs a compiler `embed(...)` feature, worth its own plan.
- Down migrations.
- Server-side sessions; WebAuthn and passkeys; SAML.
- Logs and metrics over OTLP.
- Distributed rate limiting (until Redis exists).

## 5. Owner decisions

**Confirmed by the owner on 2026-11-20: all fifteen defaults below stand.** The alternatives are kept for the record. Decision 1 still needs the owner to fetch `miniz_oxide` once (`cargo fetch`) before Milestone 4, since agents build offline.

1. **The gzip encoder.**
   - Default: add `miniz_oxide`, the pure-Rust deflate that `flate2` wraps. It is small, has no C code, and is licensed MIT, Apache-2.0 or Zlib. The owner fetches it once (`cargo fetch`), since agents work offline.
   - Alternatives:
     - `flate2` with its Rust backend (the same code, a friendlier API);
     - no crate, which ships only pre-compressed static files (4.3).
   - Dynamic Brotli is not planned either way.
2. **Time zones on Windows.**
   - Default: on Windows, only UTC and fixed offsets. `datetime.zone("Europe/London")` returns an error that says so, unless `TZDIR` points at a zoneinfo directory.
   - Alternatives:
     - bundle the IANA database in the runtime (about 400 KB, or less compressed), on every platform or on Windows only;
     - read Windows' registry zones and map them with CLDR's table.
3. **Dates and UUIDs.**
   - Default: hand-written, as section 4.5 describes.
   - The alternative is `chrono` (plus `chrono-tz` for zones) and `uuid`, which need approval and add about 1 MB of dependencies.
4. **SQLite.**
   - Default: hand-written FFI to the system's `libsqlite3`, linked only when used. The install scripts add `libsqlite3-dev` (apt), `sqlite` (Homebrew already has it) and the Windows DLL.
   - The alternative is `rusqlite` with its `bundled` feature, which compiles SQLite's C source into the runtime. It needs approval, a C compiler at build time, and about 1.5 MB per program that uses it.
5. **Metrics are opt-in**, with `server.metrics()`. Labels use the route pattern, never the path.
   - The alternative is always-on recording with only the endpoint opt-in. It is simpler for users, but it costs 2 clock reads per request in every program and would change the benchmark counts.
6. **Dashboard sign-in is HTTP Basic**, with credentials from `METRICS_USER` and `METRICS_PASSWORD` (or `METRICS_PASSWORD_HASH`). It refuses to start without them unless `public=True`. The paths are `/metrics` and `/metrics/dashboard`.
   - The alternative is a login form with a session cookie, which gives a real "sign out" and a nicer page. It needs CSRF protection, a cookie secret, and more code. Prometheus would still need Basic or a bearer token for `/metrics`.
   - A second alternative is to protect the dashboard with `auth` sign-in plus an allow-list of emails, once 4.18 exists.
7. **`.env` handling.**
   - Default: read by `config.load` (when the file exists) and `config.load_env()`. The real environment wins, and there is no `${VAR}` interpolation. `server.metrics()` itself never reads files.
   - The alternatives are interpolation (as `docker compose` does), or `.env` taking precedence.
8. **How config is decoded.**
   - Default: `config.load[T: FromJson]` with a lenient mode in the JSON decoder. It reuses `@derive(Json)`.
   - The alternative is a new `@derive(Config)`. Its errors are clearer for nested types and it can carry per-field variable names, but it is a new derive in the compiler.
9. **The status for a route timeout.**
   - Default: 503 with `Retry-After`, as Go's `TimeoutHandler` does.
   - The alternative is 504, which is meant for gateways.
10. **Templates.**
    - Default: parsed at runtime, with the context as a `ToJson` value and contextual auto-escaping. A missing variable is an error.
    - The alternative is compile-time templates checked against a typed context, which catches mistakes before running. It is a much larger compiler feature and suits a later plan.
11. **Migrations are forward-only**, as SQL files.
    - The alternative is paired `up.sql` and `down.sql` files, as `golang-migrate` uses.
12. **The rate-limit headers.**
    - Default: the IETF draft's `RateLimit-Policy` and `RateLimit`, plus `Retry-After` on a 429.
    - The alternative is the older `X-RateLimit-Limit`, `-Remaining` and `-Reset`, which more clients read today. Sending both is a third option.
13. **CSRF.**
    - Default: the `Sec-Fetch-Site` and `Origin` check, with tokens optional. `auth.install` adds it automatically.
    - The alternative is tokens always, as Django does. It is safer for very old browsers, but every form and every `fetch` needs the token.
14. **The names.** The modules are called `config`, `datetime`, `uuid`, `metrics`, `trace`, `template`, `sqlite`, `migrate` and `jobs`.
    - An alternative is to fold `datetime` into `time`. `time` keeps its float functions and gains the types.
15. **The order of the milestones**, section 6. Production HTTP first, then dates and UUIDs, then configuration and observability, and so on. Any track can move. Only the dependencies listed in each milestone are fixed.

## 6. Milestones

Each milestone follows docs/11's rules:
- **The gates, in order:**
  1. `cargo test --workspace --offline`
  2. `cargo clippy --workspace --offline --all-targets -- -D warnings`
  3. `cargo fmt --check`
  4. `sh tests/stress.sh 300`
  5. `python3 site/check_examples.py --viper target/debug/viper`
  6. `python3 bench/http/counts.py --check`, run alone
  7. `bench/run.py --check`, when the request path changed
- **Records:**
  - The decision is recorded in docs/08 from D105 on, one per milestone.
  - New error codes start at E0343, with sections in docs/errors.md.
  - The docs (docs/05, release notes, the site's section) are updated in the same commit.
- **One commit and push per milestone.**
- Each ends with a **Done** note here.

Estimates assume one agent, working in order. Tracks A to H can be reordered (decision 15). Within a track, the order matters.

### Track A: production HTTP

#### Milestone 1: foundations (about 2 days)

- Bytes bodies (`http.bytes`, `lrt_http_respond_bytes`).
- `req.remote_addr()`, `req.client_ip()` and `server.trusted_proxies`.
- The per-task request context in `fiber.rs`, inherited by `spawn`.
- `log` fields, `log.set_format`, `LOG_FORMAT`, `log.with_fields`, and the buffered output.
- `http.request_log()` with request IDs (random tokens until Milestone 6).
- `os.setenv`.
- **Tests:**
  - `tests/run/` programs: a binary body round-trip over a real socket; `client_ip` with and without a trusted proxy; JSON log lines parsed back; a request ID that appears in a log line written inside the handler and inside a task it spawned.
  - Log buffering: lines appear in order, and are flushed by `panic`, `os.exit` and SIGTERM.
- **Docs:** docs/05 (`log`, HTTP), and the site's logging section.

**Done** (D105). Everything listed, with these differences:
- The response constructor is `http.binary(data, content_type, status)`, because a function named `bytes` in `http` hides the prelude type. `response.body_bytes()` reads a body back in tests. No `lrt_http_respond_bytes` was needed: `lrt_http_respond` never treated the body as text.
- Buffered log lines are flushed each time the server's accept loop wakes (at most every 200 ms), not by a 100 ms timer. Buffering applies only while `serve` runs and standard error is not a terminal.
- Request IDs are a process counter scrambled by SplitMix64's finaliser with a random key (16 hex digits, no system call), not random tokens.
- The format variable takes the language's prefix, like `LOG`.
- The access line has no route pattern or response size yet; they come with milestone 8. A request whose handler panics gets no access line; the panic report stands for it.
- `trusted_proxies` is process-wide, set by `bind`, because a request has no reference to its server.
- Tests: `tests/run/http_binary_and_client.vi` (binary bodies both ways, `client_ip` with and without a trusted proxy and with a forged header, request IDs kept and replaced, `os.setenv`); `crates/lang_cli/tests/logging.rs` with `tests/http/request_log.vi` (text and JSON lines, the ID in a spawned task, after an error and after a panic, lines arriving while the server runs and in order at exit); unit tests for the line formats, the ID generator and the `X-Forwarded-For` walk.
- A task's request context is an optional counted string (two words), not an inline buffer: a 129-byte inline ID made every task larger whether or not it served a request.
- Gates: the workspace tests, clippy, `cargo fmt --check`, `tests/stress.sh 300` and the site's examples pass. `bench/run.py --check` passes (spawn 14 MB, as recorded). `bench/http/counts.json` was re-recorded, with the owner's approval, for one change: 16 more bytes allocated per request on every endpoint (`/plaintext` 240 to 256), with the same number of allocations. It is `Response`'s new `data` field, which moves a response from the 64-byte size class to the 80-byte one; even a one-word field does, and the other ways to carry binary bodies (raw bytes in `body`, a hidden header) break `body`'s guarantee of valid text. This is the one exception to rule 1 of section 4.20 so far.

#### Milestone 2: CORS (about 1 day)

- `http.cors` as in 4.2.
- **Tests:**
  - Preflights allowed and refused; credentials; `Vary`; a group-scoped policy.
  - The setup panic for `*` with credentials.
  - A browser check with the built-in browser against a second origin served by the same program.
- **Docs:** docs/05, and a site section on CORS.

**Done** (D106). Everything listed, with these differences:
- A function for origins is a separate `allow=` argument, since `origins` cannot be a list or a function. The checker now gives a lambda its types where an optional function is expected.
- Group-scoped preflights needed a router change: a group route with middleware keeps that middleware around a 405, and an `OPTIONS` request with `Access-Control-Request-Method` that no route accepts goes to it (D106).
- Headers for error responses: middleware records them on the request, and `Server.handle` adds them to the response it makes from an `Err`. `request_log`'s `X-Request-Id` now uses the same path.
- A preflight from a refused origin gets a 204 with only `Vary`; setup also panics for an origin with a path and for no origins.
- Tests: `tests/run/http_cors.vi` (preflights allowed and refused, actual requests, `Vary` merged with the handler's, errors, `*`, credentials, `allow`, a group policy, a group without one), two setup-panic programs, and a checker unit test. The browser check ran in the built-in browser: a page on 127.0.0.1:8601 called an API on :8602 whose `/api` group allowed it; the preflighted `POST` with a custom header succeeded and read the exposed `X-Request-Id`, a 404 was readable, and a route outside the group was blocked.
- Gates: all pass, with the counts unchanged.

#### Milestone 3: static files (about 2.5 days)

- `server.static` and `lrt_http_static` as in 4.3: the content-type table, validators, 304s, ranges, pre-compressed files, chunked sending of large files, and `fallback=`.
- **Tests:**
  - A traversal suite run over a raw socket, so the client normalises nothing: `/../`, `%2e%2e/`, `%2F`, `..%5C`, NUL, a symlink out of the root, dotfiles, and a very long path.
  - Conditional requests (each precedence case of RFC 9110 section 13.2.2).
  - Ranges: 206, 416, `If-Range`.
  - A 20 MiB file whose server memory stays flat (the GC heap and RSS, checked).
  - `HEAD`.
- **Docs:** docs/05, and a site section.

**Done** (D107). Everything listed, with these differences:
- One `GET` route serves both methods (the router answers `HEAD` from `GET` routes), and the runtime takes the path from the request rather than as an argument: `lrt_http_static` checks the raw, undecoded path, since the router's decoding would hide `%2F`.
- The `ETag` is strong, not `W/`, so that `If-Range` and `If-Match` work (D107); `If-None-Match` still compares weakly.
- Only IMF-fixdate HTTP dates are read; the obsolete forms count as no date.
- Also built: a directory named without its trailing `/` is redirected (301); the fallback is sent with `Cache-Control: no-cache`; pre-compressed files are skipped for range requests; `If-Match` and `If-Unmodified-Since` (412); setup panics for a root that is not a readable directory; static routes are left out of the API documentation.
- The response a handler returns stays ordinary: the runtime sends the file only if the response still has the file's status and no body, so middleware headers apply and middleware can replace it. Test requests get the bytes in the response, so a test `HEAD` returns a body that a real one does not.
- Tests: `tests/run/http_static.vi` (content types, indexes, the redirect, the refusals, every precondition case, ranges, `If-Range`, `br` and `gzip` with q-values, `immutable`, the fallback beside an API route); `crates/lang_cli/tests/static_files.rs` with `tests/http/static_server.vi`, over raw sockets: thirteen escapes (`..`, `%2e%2e`, `%2F`, `..%5C`, NUL, `//`, two symbolic links out, dotfiles plain and encoded, a 6 KB path) that leak nothing, a symbolic link inside the root, `HEAD` then a range on the same connection, a 304, a 3 MB range of a 20 MiB file, a client that leaves mid-file, and six 20 MiB downloads with resident memory flat (8.4 MiB) and a collected heap of 0.7 MiB at exit; unit tests for path checks, ranges, validators, encodings and HTTP dates.
- Gates: all pass, with the counts unchanged.

#### Milestone 4: compression (about 1 day; needs decision 1)

- `lrt_gzip` and `http.compress` as in 4.4.
- **Tests:**
  - Round-trips through the client, after teaching it to decode `gzip`, which it must do anyway.
  - `Accept-Encoding` with q-values and `identity;q=0`.
  - Bodies under `min_size` left alone.
  - ETags made weak.
  - `Vary`.
- **Docs:** docs/05.
- If the owner declines the crate, this milestone becomes a short note in Milestone 3's docs, and is skipped.

**Done** (D108). Everything listed, with these differences:
- `miniz_oxide` 0.9.1 gives raw deflate; the gzip header, CRC-32 and trailer are written by hand rather than through `flate2`. The decoder accepts several members and checks each trailer, and limits a decoded body to 1 GiB.
- `Accept-Encoding` is parsed by the runtime (`lrt_http_gzip_choice`), sharing the static file server's parser. `identity;q=0` (or `*;q=0`) means a non-empty body is compressed whatever its size and type.
- Also: `Cache-Control: no-transform` is honoured; `Vary` is added to every response that could be compressed, not only compressed ones; a body made by `http.binary` is compressed like a text one; errors returned as `Err` and `server.static` files are left alone (D108 says why); setup panics for a level outside 0 to 9 or a negative `min_size`.
- The client sends `Accept-Encoding: gzip` only when the caller sets no `Accept-Encoding`, and then decodes, dropping `Content-Encoding` and `Content-Length`.
- Tests: `tests/run/http_compress.vi` (gzip with `Vary` and a weak `ETag`; bodies under `min_size`, other types, refused and absent encodings; q-values, `*` and `identity;q=0`; responses with their own encoding, 304 and 206; binary bodies; a round trip through the client over a socket, with and without its own header; a static file), a setup-panic program, and unit tests for the CRC, round trips at four levels, two members, a body made by `gzip -9 -n`, corrupt, truncated and oversized bodies, and the encoding choice.
- Gates: all pass, with the counts unchanged (`http.compress` is middleware; servers without it run the same code).

### Track B: dates, times and UUIDs

#### Milestone 5: `datetime` (about 3 days)

- Everything in 4.5 except `uuid`: types, parsing, formatting, TZif zones, durations, the JSON and OpenAPI mappings, and `postgres` columns.
- `time.sleep` and the client accept `Duration`.
- **Tests:**
  - Known values: the epoch, leap days, 1900 and 2100 (not leap years), year 9999, and negative instants.
  - Round-trips of every accepted format.
  - Zones across DST transitions in `America/New_York`, `Europe/London` and `Australia/Lord_Howe` (a 30-minute DST shift).
  - A zone past its table, using the POSIX footer.
  - `ui` goldens for misuse.
- **Docs:** docs/05, and a site section.

**Done** (D110). Everything listed, with these differences:
- A `DateTime` is whole seconds plus nanoseconds, not one integer of nanoseconds, so it covers years 1 to 9999. Equality, hashing and order are by instant.
- Instants and durations combine through methods: `t.add(d)`, `t.sub(d)` and `t.since(other)` in place of `t + d` and `t - other`, because the arithmetic traits take `Self` on both sides. The compiler's error for `t + d` names the method. Durations have `+`, `-` and ordering.
- `time.sleep` keeps its float; `datetime.sleep(d)` takes a `Duration`. The client's `http.fetch` gained `timeout: datetime.Duration` (30 seconds by default). Duration constructors take `int`.
- JSON goes through a new prelude trait, `JsonString`, which any type can implement; the derive, the decoder's error paths and the OpenAPI schemas use it.
- `timestamp` columns (without a zone) are read as UTC.
- Two compiler fixes: field defaults are checked on first use (a parameter default building such a struct reported a missing field), and module-qualified explicit type arguments resolve.
- Tests: `tests/run/dates_and_times.vi` (known values from year 1 to 9999 and before the epoch, every parse and format form, DST in the three zones, the footer in 2090 and 2100, durations, JSON, the test clock), runtime unit tests for the calendar, parser, formatter, POSIX rules and the zone files, an OpenAPI `Event` type, a `query_as` line in `tests/db/database.vi`, and the `ui` goldens `datetime_misuse` and `json_string_misuse`. The zone files used are copied from tzdata into `tests/fixtures/zoneinfo`, so results do not depend on the machine's.
- Gates: all pass, with the counts unchanged.

#### Milestone 6: `uuid` (about 0.5 day)

- `v4`, `v7` (monotonic), `parse`, JSON, OpenAPI, and path parameters.
- `request_log` switches to `uuid.v7()`.
- **Tests:**
  - The version and variant bits; v7 ordering across 100,000 calls in one millisecond; parse errors.
  - A path parameter returning a 400.
- **Docs:** docs/05.

**Done** (D111). Everything listed, with these differences:
- Path parameters are not special to `uuid`: any type implementing `JsonString` can be a path or query parameter, so `datetime.Date` and users' own types work too. Its parse error is the 400's message, and OpenAPI shows its string schema (`format: uuid`, `date`, `date-time`).
- A `Uuid` stores its lowercase text, so the derived order is the bytes' order. `version()`, `bytes()`, `time()` (for v7), `from_bytes` and `nil()` are added.
- `v7` uses a 42-bit counter and reads `datetime`'s clock, so `set_clock_for_tests` fixes it. Random bytes come from a per-thread 4 KiB pool.
- Request IDs from `request_log` are v7 UUIDs; the SplitMix64 generator of D105 is removed.
- Tests: `tests/run/uuids.vi` (version and variant bits; 100,000 v7 UUIDs in one fixed millisecond, in order; the clock going back; parse errors; bytes, sorting, sets and JSON; `uuid.Uuid` and `datetime.Date` path and query parameters with their 400s), runtime unit tests (bits, order across 100,000 calls, counter overflow, parsing), an OpenAPI route with a `Uuid` path parameter and a `Date` query parameter, the `ui` golden `uuid_misuse`, and the request ID checks in `tests/run/http_binary_and_client.vi` and the logging test.
- Gates: all pass, with the counts unchanged (the benchmark server does not use `request_log`).

### Track C: configuration and observability

#### Milestone 7: `config` and `.env` (about 2 days)

- `config.load_env`, `config.load[T]`, the lenient decoder and `config.Secret` (4.6). Settle decision 8 first by reading `json.rs`.
- **Tests:**
  - The `.env` grammar: every quoting case, comments, `export`, multi-line values, CRLF files, and the error with a line number.
  - Precedence.
  - The one error listing every problem.
  - `Secret` in `str()`, f-strings and logs.
- **Docs:** docs/05, a site section, and a `.env.example` in the examples that need one. `.gitignore` gains `.env` in example directories.

**Done** (D112). Everything listed, with these differences:
- Decision 8 kept its default: `load[T: FromJson]` decodes with the derived JSON code in a lenient mode, a flag the reader takes when it starts. Recovery happens only at the top level, so a nested value's problem is reported against its variable with its path (`LIMITS at burst must be an integer, got "x"`).
- `load_env`'s `path` defaults to `None` rather than `".env"`, which is how a missing default file is fine while a missing explicit one is an error.
- Double-quoted values also read `\r` and `\t`, and keep any other backslash. A list may be JSON as well as comma-separated, and a struct or `dict` field is read as JSON.
- `Secret` is `***` in JSON too, so an encoded settings struct does not leak it; `config.secret(value)` makes one in code. `ConfigError` has the list of problems as well as the joined message.
- `os.environ()` is added, which `load` needs.
- The `google_login` example reads a `.env` with `load_env` and has a `.env.example`; `.gitignore` has `examples/*/.env`.
- Tests: `tests/run/config_env.vi` (named so that it does not hide the module) with fixtures in `tests/fixtures/config/`: every quoting case, comments, `export`, multi-line values, a CRLF file with a byte order mark, line-numbered errors, precedence in both directions, seven problems in one error, comma and JSON lists, boolean spellings, `Duration`, `Uuid`, `dict` and optional fields, and `Secret` in `str()`, f-strings, a derived `Display` and JSON. Runtime unit tests for the `.env` grammar and for lenient reads; the `ui` golden `config_misuse`. A log line is an f-string, so the f-string case covers logs.

#### Milestone 8: metrics, health and readiness (about 3 days; after 7)

- **The runtime registry:** sharded counters and histograms, route indices from `compose` and `dispatch`, the process and runtime gauges, and `METRICS_ON`.
- **Application metrics** (`metrics.counter`, `gauge` and `histogram`, with `time()`).
- `server.metrics()` serving the Prometheus text, with Basic authentication and the startup refusal. The dashboard is not part of this milestone.
- `server.health()` with checks, `drain_delay`, and readiness flipping on SIGTERM.
- `bench/http/counts.py --observed` and `counts_observed.json`.
- **Tests:**
  - The text format, parsed by a small checker in the test, checked against the exposition format's rules: names, label escaping, histogram `_bucket`, `_sum` and `_count`, and `le="+Inf"`.
  - The counts under concurrent load: 100,000 requests over 64 connections, totals exact.
  - The `unmatched` label.
  - The cap of 1000 label values.
  - 401s without credentials; the startup panic's text; the constant-time comparison (a unit test that it uses the HMAC path).
  - `/readyz` failing a check, and going to 503 during drain.
- **Gate:** the plain counts unchanged; the observed counts within the budget of 4.20; the throughput figure recorded.
- **Docs:** docs/05, a site section "Metrics and health", and docs/web-performance.md (the observed-mode numbers).

**Done** (D113). Everything listed except the dashboard's parts, with these differences:
- Route slots are given when a route is added (`new_route`, through `lrt_http_route_slot`), interned by method and pattern, rather than when `compose` runs. `dispatch` stores the slot with `lrt_http_set_route`, with the route's flags (left out of the metrics; left out of the access log) in the high bits.
- The counters are 16 shards that threads take in turn when they first record, rather than one array per worker thread. Each route's cells are padded to 64 bytes; the counts by route, method and status are a 4096-entry table per shard whose keys are claimed with a compare-and-swap.
- The duration histogram is labelled by method and route. `http_response_size_bytes` is a summary (`_sum` and `_count`); `HEAD` and 304 responses count 0 bytes. `process_cpu_seconds_total` is added, and `runtime_unrecorded_requests_total` appears only if a series found no room.
- `METRICS_PASSWORD_HASH` and the `dashboard=` argument are left to Milestone 9, which settles the hash's format with `viper hash-password`.
- **The drain.** SIGTERM (or `server.stop()`) asks the server to stop; with `drain_delay` above 0 it goes on serving for that many seconds, logging `draining for N s before stopping`, while `/readyz` answers 503 `{"server":"draining"}`. SIGINT still stops at once.
- **The checks** run each in its own task with one shared 2 s deadline; one that has not answered is reported as `did not answer within 2 s`. `checks` must be a dict literal of functions (`check/http.rs`), because the standard library spawns them: the sharing check now trusts a standard-library module to spawn the function values it was given, while a program doing the same still gets E0338.
- The metrics route is left out of the metrics and the documentation but is in the access log; `/healthz` and `/readyz` are left out of all three. Access log lines gain a `route` field, and `req.route()` gives the pattern.
- **The cost, measured.** The plain counts are unchanged. Metrics alone add exactly the budget: +2 `clock_gettime`, no allocation and no system call per request, and the rate at 256 connections stays within 1.3% of plain (3 alternating runs of 5 s, `bench/http/server.vi ... metrics`):

  | Endpoint | plain req/s | metrics on | change |
  |---|---|---|---|
  | `/plaintext` | 1,087,529 | 1,100,665 | +1.2% |
  | `/json` | 1,090,881 | 1,089,442 | -0.1% |
  | `/items` | 598,631 | 590,693 | -1.3% |
  | `/users/42` | 1,008,261 | 998,460 | -1.0% |
  | `POST /orders` | 966,384 | 965,032 | -0.1% |

  The observed mode, which adds the access log of Milestone 1, is **over** 4.20's budget: +6 clock reads, about 13.6 Rust heap and 17 collected allocations (2.8 KB) per request, still no system calls, and a rate 31% to 42% below plain (`/plaintext` 1,025,701 against 599,063). All of the excess is the access log (the request ID, the line's fields and its formatting), not the metrics; `counts_observed.json` records it so that it cannot grow unnoticed. Making the access log cheaper is a follow-up of its own.
- Tests: `crates/lang_cli/tests/metrics.rs` parses the text with a checker of the format's rules (names, label escaping, `# HELP` and `# TYPE`, histogram `_bucket`, `_sum` and `_count` with `le="+Inf"`), and checks exact totals after 100,000 requests pipelined over 64 connections, errors by kind, the `unmatched` label, labels of the program's metrics, 401s and Basic authentication, the access log's `route`, the startup panic's text, and `/readyz` failing a check and answering 503 through a drain on SIGTERM (`tests/http/metrics_server.vi`). `tests/run/metrics_module.vi` covers the module, the cap of 1000 sets and the label panic; runtime unit tests cover the HMAC comparison, names, registering again, the text format and series keys.

#### Milestone 9: the metrics dashboard (about 3 days; after 8)

- The history ring and its sampling thread; the recent-errors and slowest-requests records; `data.json`.
- The embedded page, stylesheet and script, as in 4.8, with `lrt_dashboard_asset` and ETags.
- `METRICS_PASSWORD_HASH` and `viper hash-password`.
- Lock-out after failed attempts. The warning about plain HTTP.
- **Tests:**
  - The page, the stylesheet and the script are absent from an optimised binary that does not call `server.metrics`, and present in one that does. Search the binary for a phrase of the script, as `carries_viewer` does in `crates/lang_cli/tests/executables.rs`.
  - `data.json` with `since`.
  - The recent errors leave out query strings.
  - PBKDF2 against an RFC 6070-style vector for SHA-256.
  - The lock-out after 10 failures.
  - **A visual check in the built-in browser:** load the page with a running example under load (`bench/http` client for 30 s), and take screenshots in light and dark mode and at phone width.
- **Docs:** docs/05; the site section with a screenshot description; and a new `examples/metrics_dashboard/` with a `.env.example`, a README and tests.

**Done** (D114). Everything listed, with these differences:
- The routes are `<path>/dashboard` (and with a trailing `/`), `/dashboard.css`, `/dashboard.js` and `/dashboard/data.json`, each checked for credentials itself rather than through middleware, since they are added as special routes like the metrics. The stylesheet and script keep ETags with `no-cache`; the page and the data are `no-store`. The CSP adds `frame-ancestors 'none'`.
- The lock-out is a table of 4096 addresses in the metrics module; requests without an `Authorization` header are not counted, because a browser always tries without one first. It returns a 429 with `Retry-After` set to the seconds left.
- The p90 is charted and p95 shown in the tiles and routes, as planned; the "Concurrency" chart has tasks with in-flight requests and connections, and "Memory" has the heap and resident memory, so no chart mixes units. Histograms of the program are shown as their counts.
- The interval can be shortened with `VIPER_METRICS_INTERVAL_MS` (at least 50 ms), for tests.
- The page fetches `data.json` against `location.origin`: when the page's address carries a user name and password, as a test browser's may, `fetch` refuses a relative URL.
- **The visual check** ran `tests/http/metrics_server.vi` (optimised) under `bench/http/load.go` for six rounds of 10 s (32 connections on `/items/{id}`, 4 on `/tag`, three panics a round), about 200,000 requests per second, and looked at the page at 1280 by 900 in light and dark mode and at 375 by 812: no overflow at phone width, the tooltip follows the crosshair, the errors list the panics without their query strings, and the slowest requests are listed with IDs.
- Tests: `crates/lang_cli/tests/metrics.rs` fetches the page, assets (with 304s) and `data.json` with and without credentials, checks the headers, that `since` returns only newer samples, that recent errors drop the query string, the lock-out and its log line, a hash from `hash-password` in `METRICS_PASSWORD_HASH`, and the refusal of a malformed one; `executables.rs` checks the page is absent from a server without metrics and present with them; runtime unit tests cover PBKDF2 against SHA-256 vectors in the style of RFC 6070, the lock-out window, quantile interpolation, JSON escaping and the assets' sizes; `examples/metrics_dashboard` has four tests of its own.

#### Milestone 10: traces (about 2.5 days; after 6)

- `trace.init`, `traceparent` in and out, the server, client and `postgres` spans, `trace.span` blocks, and the batch exporter.
- **Tests:**
  - A fake OTLP collector in `crates/lang_cli/tests/` (a `TcpListener` like the fake Google in `oidc.rs`), which records the JSON payloads.
  - Assertions: parent and child links across `http.fetch`; attributes; the sampling ratio of 0 and 1; dropping when the collector is down; a flush at shutdown.
  - Log lines carry `trace_id`.
- **Docs:** docs/05, and a site section.

**Done** (D115). Everything listed, with these differences:
- The server span is started by the runtime around the handler chain, not by middleware, so every request of a server is traced once `trace.init` has run; `http.request_log(trace=True)` is a way to turn it on from the environment alone. The metrics' and health checks' routes are left out, as they are of the metrics.
- `tracestate` is passed through with the trace, to fetches and back out, unchanged.
- The exporter's health is on `/metrics` as `trace_spans_exported_total` and `trace_spans_dropped_total`, and a failing collector is logged once, then once more when it recovers.
- The 5 s interval can be shortened with `VIPER_TRACE_INTERVAL_MS` (at least 10 ms), for tests.
- PostgreSQL spans are covered by a unit test of how statements are named only: no database was available where this milestone was built. The existing postgres tests run through the wrapped functions when one is.
- Tests: `crates/lang_cli/tests/trace.rs` runs `tests/http/trace_server.vi` against a fake collector that parses the OTLP/JSON: a server span continuing an incoming `traceparent`, a fetch to the server's own route as a client span whose `traceparent` names it and whose server span is its child, nested `trace.span` blocks with attributes and a failure, a panic's span, the route's pattern as the name, `trace_id` and `span_id` in the handler's and the access log's lines, a ratio of 0 recording nothing but passing the trace on, spans held until the server stops and then flushed, spans dropped and counted while the collector answers 503 (with the warning and the recovery line), and the queue's cap. Runtime unit tests cover parsing `traceparent`, naming statements, the JSON encoding, and the endpoint and ratio settings; a log unit test covers the IDs in text and JSON lines.

### Track D: handling requests

#### Milestone 11: lazy bodies and per-route limits (about 3 days)

- 4.10's first part: the 64 KiB eager threshold, pending bodies, route, group and server `max_body`, `100-continue`, draining, and the limits on header count and target length.
- `max_body=` on route annotations, checked in `check/http.rs`.
- **Tests:**
  - A 413 at exactly the limit plus one, for both `Content-Length` and chunked bodies.
  - `100-continue` sent only when the body is read.
  - A pipelined request after a pending body that was drained.
  - A connection closed after an undrained large body.
  - The stress run (300) passes.
- **Gate:** the counts unchanged, since every benchmark body is under 64 KiB.
- **Docs:** docs/05.

**Done** (D116). Everything listed, with these differences:
- A route's limit is carried in `Route.label` (times 2^24) and stored by the call that already tells the runtime which route took the request; a group's is folded into its routes when they are registered.
- Eagerly read bodies are checked against the route's limit as well, since a route or group may allow less than the server. Only a body that was not all read closes the connection after a 413.
- `req.body()` returns `bytes`, not a `Result`: past the limit it is empty, and the runtime replaces whatever the handler answered with the 413, as it does when the handler ignores the error from `req.text()`.
- A connection that closes with body bytes still arriving reads and drops them for up to two seconds first (a lingering close), so the client reads the 413 or the `Connection: close` response instead of a reset.
- A pending body's read allows 30 seconds for each wait rather than 30 seconds in all.
- The head limits are `server.max_headers` (100) and `server.max_url` (8 KiB); 414 is counted under `too_large` like 413 and 431.
- Test requests check a route's or group's `max_body` in `req.text()`.
- Tests: `crates/lang_cli/tests/limits.rs` runs `tests/http/limits_server.vi` with the server's, a group's, a group route's, a route's past 64 KiB and an annotation's limits, at each limit and one past it, by `Content-Length` and chunked; `req.body()` and a handler that swallows the error both still answer 413; `100 Continue` sent only when the body is read, a 413 before it, and a close when it is never read; pipelined requests after drained `Content-Length` and chunked bodies; a close after a 3 MiB body that is not read; and the header and target limits at and past them. `tests/run/http_max_body.vi` covers test requests, and `tests/ui/route_annotation_errors.vi` a bad `max_body=`. The stress run passes, and the counts are unchanged.

#### Milestone 12: forms and uploads (about 3 days; after 11)

- The multipart parser in Rust.
- `req.form()`, `Upload`, and the `Form[T]` and `Upload` parameters with error E0343.
- OpenAPI request bodies; the viewer's file input.
- `test_request(form=..., files=...)`.
- **Tests:**
  - Multipart edge cases: CRLF and lone LF, a boundary inside the data, `filename*`, empty parts, a part spilling to disk, the cleanup of temporary files.
  - The limits.
  - A real upload from the built-in browser through the documentation viewer.
  - `ui` goldens for E0343.
- **Docs:** docs/05, docs/errors.md (E0343), and a site section.

**Done** (D117). Everything listed, with these differences:
- The parsed form is `http.FormData`; `http.Form[T]` is only the handler parameter, whose decoded fields are its `value`. `FormData` also has `file(name)` and `names()`.
- The body is read whole, under the route's `max_body`, before it is split; files over 1 MiB are then written to `upload_dir`. Parsing while the body arrives is left for later.
- `Upload.open()` is left out: the `io` module has no binary file handle yet. `bytes()`, `text()` and `save(path)` are there.
- The lenient decoder reads `on` and `off` as booleans, for checkboxes (and so `config.load` does too).
- The viewer sends a form without files urlencoded, and lists such routes with both media types.
- Tests: unit tests in `multipart.rs` (CRLF and lone LF, a preamble, delimiter-like lines in data, `filename*` in UTF-8 and ISO-8859-1, Windows paths and `..`, empty fields, empty file inputs, every malformed-body error, the three limits, spilling and removal); `crates/lang_cli/tests/forms.rs` runs `tests/http/upload_server.vi` over the wire (decoding, a 1.5 MB file waiting on disk during the request and gone after, `save`, lists and optional files, each limit, and the documented bodies); `tests/run/http_forms.vi` covers test requests; `tests/ui/form_errors.vi` gives E0343; `core.test.js` covers the viewer's form fields, urlencoded and multipart requests and curl `-F`. A 1.5 MB file was uploaded from the built-in browser through the documentation page's "Try it out" (`tests/http/docs_page_check.md`).

#### Milestone 13: timeouts and cancellation (about 3 days; after 5)

- The per-task deadline and cancel flag, checked at every park point; `Cancelled` errors.
- Route, group and server `timeout`.
- `RDHUP` detection of disconnects.
- Per-call client timeouts. `req.deadline()`, `req.cancelled()`, `http.check_cancelled()`.
- **Tests:**
  - A handler sleeping past its timeout gets a 503, and its later work does not run.
  - A `postgres` query cancelled by a deadline (with the test database, as the existing postgres tests do).
  - A client that closes mid-request cancels the handler (seen through a channel).
  - Pipelined requests are not taken for disconnects.
  - A client `connect_timeout` against a blackholed address (a listening socket that never accepts, with a full backlog).
  - The stress run.
- **Gate:** the counts unchanged. The checks are one load of the context per park, and the bench measures it.
- **Docs:** docs/05, docs/04 (runtime), and a site section.

Done (D118):

- A task's context carries a deadline and a cancel reason. `sleep`, channel receives, socket reads and writes (so client calls and `postgres` queries) and the database pool's wait look at the reason before they wait: one load, and a look at the coarse clock only when there is a deadline. The timer thread's quarter-second sweep marks tasks past their deadline and wakes them.
- `timeout` on `server.route` and the verb methods, `group.timeout`, `server.timeout`, and `timeout=` in route annotations. A handler past its timeout gets a 503 with `Retry-After: 1`, the `timeout` error kind in the metrics, and status 503 in `request_log`. Tasks it spawned inherit the deadline.
- A hang-up is seen through `EPOLLRDHUP`, checked with a zero-timeout `poll` and tied to the socket's registration, so a recycled descriptor never inherits one. Pipelined requests are data, not a hang-up; a half-close after a request is one, as in Go. The handler is cancelled, its response is not written, and `client_closed` is counted.
- A `postgres` query stopped by a cancel sends a CancelRequest, so the server stops it too, and the connection is dropped from the pool.
- `http.fetch` takes `connect_timeout=`; client errors name the limit that ran out ("timed out connecting to ... (connect_timeout=0.3s)").
- `req.deadline()`, `req.cancelled()` and `http.check_cancelled()`, which is a 503 after a deadline and a 499 after a hang-up. Code that never waits is not interrupted.
- Tests: `crates/lang_cli/tests/cancel.rs` runs `tests/http/cancel_server.vi` (route, group, annotation and server timeouts, a spawned task, a loop that checks, a hang-up seen through a channel, pipelining and a half-close, `connect_timeout` against a listener with a full backlog, and, with the test database, a query stopped at the deadline); `tests/run/http_cancel.vi` covers test requests and code outside a request; `tests/ui/route_annotation_errors.vi` covers bad `timeout=` values.

#### Milestone 14: rate limiting (about 1.5 days; after 1)

- `http.rate_limit` as in 4.12, the `rate_limit=` short form on annotations, and the sweep and key cap.
- The dashboard's lock-out moves onto it.
- **Tests:**
  - The burst, then refusal, then refill over a fixed test clock.
  - The headers.
  - The key cap with 200,000 distinct keys, with memory bounded.
  - Concurrency: no more than `burst` allowed out of 1000 simultaneous requests.
- **Docs:** docs/05.

Done (D119):

- `http.rate_limit(rate="", per_second=0.0, burst=0, key=None, name="default")`: a token bucket per key in the runtime, over 64 shards; full buckets dropped once a minute per shard; at most 100,000 keys, the longest unused evicted first; long keys hashed.
- Headers as in draft 12 of the IETF RateLimit fields (checked; it uses `a` and `w`, not the earlier `r` and `t`): `RateLimit-Policy: default;q=40;w=2` and `RateLimit: default;a=37;w=1`. A refusal is a 429 error, so `on_error` applies, with `Retry-After`.
- `rate_limit="5/minute"` on annotations, checked by the compiler, and on `server.route`, the verb methods and groups; the OpenAPI description lists the 429.
- The dashboard's lock-out uses a limiter of 10 a minute.
- The limiter's clock follows `datetime.set_clock_for_tests`.
- Tests: unit tests in `ratelimit.rs` (burst, refusal and refill on a fixed clock, the lock-out, the key cap, the sweep, long keys, 16 threads against a burst of 50); `tests/run/http_rate_limit.vi` (the same through middleware with a fixed clock, the headers, keys, a route's limit inside the server's, the annotation, `on_error`, OpenAPI); `crates/lang_cli/tests/rate_limit.rs` over the wire (1000 requests at once let exactly 50 through, the headers, and 400,000 distinct keys: memory grew 22 MB for the first 200,000 and under 1 MB for the next); `tests/ui/route_annotation_errors.vi` covers bad rates.

### Track E: responses

#### Milestone 15: streaming and Server-Sent Events (about 2.5 days)

- `http.stream`, `http.sse`, heartbeats, `Last-Event-ID`, and the exclusions from compression and timeouts.
- The dashboard's live mode.
- **Tests:**
  - Chunked framing read by the client.
  - A 1 GiB stream with flat memory.
  - Disconnect detection.
  - SSE parsing in the built-in browser's `EventSource` (the dashboard).
  - `test_request` collecting a stream.
- **Docs:** docs/05, and a site section.

Done (D120):

- `http.stream(req, body, content_type=..., status=200)` and `http.Stream` (`write`, `write_bytes`, `flush`, `closed`): chunked over HTTP/1.1, ended by closing over HTTP/1.0, 16 KiB buffering, the body cut short when the function fails. The function runs after the middleware.
- `http.sse(req, handler, heartbeat=15.0)` and `http.Events` (`send` with `event`, `id` and `retry`; `comment`; `last_id`; `closed`), a `: ping` heartbeat, and `http.StreamClosed` once the client has gone.
- Streams are left out of `http.compress`, and the route's timeout stops when a stream starts; a hang-up still cancels it.
- `server.handle(http.test_request(...))` runs the stream and gives what it wrote as the body.
- The metrics dashboard follows `path + "/dashboard/events"` with `EventSource`, resuming by sample number, and polls `data.json` when the stream fails.
- Not done: `text/event-stream` in the OpenAPI document (D120).
- Tests: `tests/run/http_stream.vi` (collection, no compression, the SSE format, `Last-Event-ID`, binary data, a failed stream); `crates/lang_cli/tests/stream.rs` over the wire (chunked framing checked chunk by chunk and read by the language's client; 1 GiB with a peak of 12.5 MB resident; events and heartbeats noticing a hang-up; a stream outliving its route's 0.3 s timeout; a failed stream without its last chunk; HTTP/1.0; the dashboard's events resuming after `Last-Event-ID`). The dashboard checked in the built-in browser: live mode, pause and resume, and the fallback to polling.

#### Milestone 16: templates (about 4 days)

- The `template` module as in 4.14: the lexer, the parser, inheritance, filters, contextual escaping checks, reload, and `http.render`.
- **Tests:**
  - An escaping suite: each context, each `raw` and `json` case, and `javascript:` URLs.
  - Inheritance three levels deep; include cycles (an error).
  - Error messages with template line numbers.
  - Rendering speed: a 1000-row table in under 5 ms, recorded.
- **Docs:** docs/05, a site section, and an example page in `examples/`.

Done (D121):

- `template.load_dir(path, reload=False)` and `template.from_strings(sources)` give `template.Templates`; `render(name, context)` takes anything `ToJson`. `http.render(pages, name, context, status=200)` makes the HTML response; a failing template is a logged 500.
- The syntax of 4.14 (output, `if`/`elif`/`else`, `for`/`empty` with `loop`, `set`, `include`, `extends`/`block`, comments, white-space trimming) and its filters (`upper`, `lower`, `title`, `trim`, `length`, `default`, `join`, `date`, `json`, `url`, `raw`), with `==`, `!=`, `<`, `<=`, `>`, `>=`, `and`, `or`, `not`, `in`, `not in`, and `+`, `-`.
- Contextual escaping checked at load time: a `{{ }}` in `<script>`, `<style>`, or an `on...=` or `style=` attribute needs `| json` (or `| raw`); one in an unquoted value, between attributes or in a tag name is refused. URL attributes have their whole value replaced by `#unsafe` when a scheme other than `http`, `https` or `mailto` comes out, including one assembled from several `{{ }}`.
- Load errors for unknown `extends` and `include`, cycles of either, a block or include in a script, branches that end in different places, duplicate blocks, text outside the blocks of a child. Render errors name the template, line and expression.
- `reload=True` rescans and loads again what changed.
- Runtime: `io.is_dir` and `io.modified_ns`. Nothing else in Rust.
- Speed: a 1000-row table (148 KB) renders in 1.2 ms in a release build (best of 20, `time.monotonic`), 7.5 ms in a debug build; the limit was 5 ms in a release build.
- Tests: `tests/run/templates.vi` (52 outputs and errors: every statement and filter, three levels of inheritance, includes, each escaping context, the URL schemes in their spellings, and every load error with its line); `tests/run/template_speed.vi` (the 1000-row table, in the stress modes); `crates/lang_cli/tests/template.rs` over the wire (nested directories, an edit, a new file, a broken edit as a 500 whose details are only in the log, a fix) and `examples/recipes`' five tests.
- Docs: docs/05 ("Templates"), `examples/recipes`, and a site section.

### Track F: data and background work

#### Milestone 17: SQLite (about 3 days; needs decision 4)

- `crates/lang_rt/src/sqlite.rs` (FFI and the connection thread), `stdlib/sqlite.lang`, the linking rule in the driver, and the install-script changes.
- **Tests:**
  - CRUD round-trips; `query_as`; transactions and rollback on error.
  - Concurrent writers under WAL; `busy_timeout`.
  - `DateTime`, `Uuid` and `bytes` columns.
  - A program that doesn't import `sqlite` does not link it (checked with `ldd`).
- **Docs:** docs/05, docs/building.md (the system library), and the install scripts.

Done (D122):

- `crates/lang_rt/src/sqlite.rs`: FFI to `libsqlite3` through `dlopen` (D122 explains why not `-lsqlite3`), a thread per database, a transaction that owns the connection, typed parameters and cells, named error codes. `stdlib/sqlite.lang`: `open`, `Db` (`query`, `query_row`, `execute`, `insert`, `script`, `transaction`, `commit`, `rollback`, `close`), `Row`, `query_as`, `query_one_as`, `blob`.
- WAL, `busy_timeout=5000` and `foreign_keys=ON` on every file database; `INTEGER`, `REAL`, `TEXT`, `BLOB` and `NULL` map as planned, `BOOLEAN` columns read as bool in `query_as`, and `DateTime` and `Uuid` are text.
- Install: `libsqlite3-0` in `scripts/install.sh`, an optional line in `scripts/check-deps.sh`, and docs/building.md.
- Tests: three unit tests (error names, rows as JSON, a real in-memory database); `tests/run/sqlite_database.vi` (CRUD, `query_as`, each type, `DateTime` and `Uuid`, commit, abandoned, failed and nested transactions, every error code, parameter checks, eight tasks on one database, a transaction that holds the connection, closing); `crates/lang_cli/tests/sqlite.rs` (two processes with two connections each writing 600 rows to one WAL file; the pragmas; no `libsqlite3` in the executable's `NEEDED` list).

#### Milestone 18: migrations (about 2 days; after 17)

- `migrate.apply` and `status` for both databases; checksums; the advisory lock; `no-transaction`.
- `viper migrate new` and `status`.
- **Tests:**
  - Applying twice does nothing the second time.
  - An edited file is an error.
  - A failing migration rolls back and stops.
  - Two processes starting together on Postgres: one applies, the other waits.
  - The CLI.
- **Docs:** docs/05, and a site section.

Done (D123):

- `stdlib/migrate.lang`: `apply`, `status`, `files`, the `Backend` trait, `Migration`, `Applied`, `Status`, `MigrateError`, and the checks. `postgres.Db` and `sqlite.Db` implement `Backend` in their own modules (`impl migrate.Backend for Db`, which the checker now allows for a trait of another module).
- PostgreSQL: a session advisory lock on a connection outside a transaction (`lrt_pg_session`), each file in its own transaction with its record, `no-transaction` for `create index concurrently`. SQLite: `BEGIN IMMEDIATE` per file, with the version checked again inside it.
- `viper migrate new <name>` and `viper migrate status [--dir] [--database]` (`crates/lang_cli/src/migrate.rs`); `io.make_dir` and `io.remove_dir`.
- Fixed: a finished SQLite transaction rolled back again by `with` left an error that `io.read_file` then reported; two processes creating one SQLite file could fail switching to WAL.
- Tests: `tests/run/migrations_sqlite.vi`, `tests/db/migrations_postgres.vi`, `crates/lang_cli/tests/migrate.rs` (two processes together on both databases; the command line), checked against a real PostgreSQL 17.

#### Milestone 19: jobs and schedules (about 2.5 days; after 5, and 8 for metrics)

- `jobs.every`, `jobs.cron` (the parser and zone-aware next-run), `Queue[T]` with retries and backoff, and shutdown.
- `jobs.run_due_for_tests`.
- **Tests:**
  - Cron next-run tables: across DST gaps and overlaps, month ends, and Feb 29.
  - No overlap.
  - Retries with backoff (a fixed test clock).
  - Bounded `submit`.
  - Draining at SIGTERM.
- **Docs:** docs/05, and a site section.

Done (D124):

- `stdlib/jobs.lang`: `every`, `cron`, `parse_cron`, `next_run`, `Schedule` (with `tick`, `stop` and counters), `run_due_for_tests`, `queue[T]` and `Queue[T]` (`submit`, `wait`, `close` and counters), `backoff_delay`. The runtime adds `lrt_jobs_stopping` and `lrt_jobs_running`, and `serve` waits for running jobs. The checker requires the functions given to `jobs` to be shareable.
- Deviations from the plan: `jobs.queue[T](handler, ...)` is a function rather than `Queue[T](...)`, and schedules made with `start=False` are driven by `tick` or `run_due_for_tests(schedules, now)`; D124 says why.
- Tests: `tests/run/jobs_cron.vi` (month ends, Feb 29, weekdays, names, steps, New York and London gaps and overlaps, Kolkata, the errors), `tests/run/jobs_queues.vi` (fixed-clock schedules, skipped runs, a running schedule that does not overlap, retries, the failure metric, a bounded queue, the backoff table), `tests/ui/jobs_sharing.vi`, `crates/lang_cli/tests/jobs.rs` (SIGTERM: the running job finishes, the queue is dropped and counted, the schedule stops).

### Track G: sign-in

#### Milestone 20: CSRF (about 1.5 days; after 1)

- `http.csrf` with the origin checks and optional tokens; `csrf_field` in templates (after 16, or added when 16 lands); `auth.install` adding it.
- **Tests:**
  - Every `Sec-Fetch-Site` value; `Origin` missing, same, cross and trusted.
  - `Authorization` exempt.
  - Tokens: valid, missing, wrong and forged.
  - The Google example still passes its tests.
- **Docs:** docs/05, and the site's sign-in section.

Done (D125):

- `http.csrf(trusted_origins, tokens, secret, secure)` with the `Sec-Fetch-Site` and `Origin` checks, signed double-submit tokens, `http.csrf_token(req)`; `{{ csrf_field() }}` in templates with `http.render(..., req=req)`; `auth` providers' `install(server, csrf=True)` adds it.
- `same-site` is refused as well as `cross-site` (D125 says why).
- Tests: `tests/run/csrf_protection.vi`; the Google tests pass unchanged.

#### Milestone 21: generic OIDC and presets (about 3 days)

- Discovery, `crypto.verify_es256`, `auth.oidc`, and the Microsoft, Okta and Auth0 presets.
- `install_all`, `User.provider`, and the renamed security scheme with its alias.
- **Tests:**
  - The fake provider in `oidc.rs` gains a discovery document and an ES256 key.
  - Discovery with an issuer mismatch refused.
  - Microsoft's `{tenantid}` issuer template.
  - Two providers on one server.
  - The Google example unchanged.
- **Docs:** docs/05, the site, docs/13's section 4.7 ("later" items) marked done.

Done (D126):

- `auth.oidc(issuer, name=...)` with discovery, `crypto.verify_es256`, ES256 and RSA keys in the same key set, and issuer checks including `{tenantid}`; `microsoft`, `okta`, `auth0`, `oidc_for_tests`, and `install_all`.
- `User.provider` and `<name>:<sub>` IDs (Google's unchanged); per-provider flow cookies; the scheme `session` with the `googleSession` alias.
- Tests: `sign_in_with_discovered_providers` and `tests/oidc/providers.vi`; the Google tests pass unchanged.

#### Milestone 22: GitHub (about 1.5 days; after 21)

- `auth.github` as in 4.18.
- **Tests:**
  - A fake GitHub API (user and emails, with an unverified primary email refused).
  - The `User-Agent` sent.
  - A real-GitHub manual check, like docs/13's real-Google check, which needs the owner's OAuth app.
- **Docs:** docs/05, and an example.

Done (D127):

- `auth.github` and `auth.github_for_tests`: OAuth 2 without an ID token, the account read from `/user` and `/user/emails`, the primary verified email, `User.id` as `github:<number>`.
- Tests: `sign_in_with_github` and `tests/oidc/github.vi` against a fake GitHub API that requires a `User-Agent`; the example `examples/github_login`.
- Not done: the manual check against a real GitHub OAuth app, which needs the owner's app.

### Track H: lower priority

#### Milestone 23: response caching and ETags (about 1.5 days)

`http.etag()`, `http.cache(...)` and `Response.cache_control`, as in 4.19.

Done (D128):

- `http.etag(weak)`, `http.cache(ttl, max_bytes, max_entry_bytes, key)` and `Response.cache_control(...)`.
- Responses with `Set-Cookie`, `Vary`, `no-store` or `private` are not cached; requests with `Authorization` or `Cookie` bypass the cache unless a `key` is given.
- Tests: `tests/run/response_cache.vi`; counts unchanged.

#### Milestone 24: the router, if measured (about 2 days, or nothing)

- First, add a 200-route endpoint to `bench/http` and measure it.
- Build the radix tree only if 4.19's threshold is crossed. Otherwise, record the measurement in docs/web-performance.md and close the milestone.

Done (D129):

- Measured first: 200 extra parameterised routes cost 22.8% of a `GET /users/42` request (0.76 µs of 3.34 µs on one CPU), over the 5% threshold, so the tree was built.
- `Server` finds routes through a tree of path segments, built in `compose`; with it the 200 routes cost 0.3%. The first-added-wins rule is kept.
- `bench/http/routes.py`, the `routes200` mode of `bench/http/server.vi`, and `docs/web-performance.md` "Many routes". Tests: `tests/run/router_tree.vi`; counts unchanged.

#### Milestone 25: Redis (about 3 days)

- A RESP3 client with pooling; `GET`, `SET`, `INCR`, `EXPIRE`, pub/sub, and pipelines.
- Redis-backed stores for `rate_limit` and `jobs.Queue`.

### What will be built, in total

About 55 days of work through Milestone 22, and 6.5 more for track H:
- nine new modules (`config`, `datetime`, `uuid`, `metrics`, `trace`, `template`, `sqlite`, `migrate`, `jobs`);
- about twenty additions to `http`;
- three to `auth`;
- one compile error;
- an embedded dashboard;
- documentation and examples for each.

## 7. Risks and open questions

- **Cancellation is cooperative.** A handler stuck in a CPU loop cannot be stopped. A timeout answers the client, but the task runs on and holds its memory. This is documented, with `check_cancelled()`. Preemption is not planned.
- **Blocking file reads.** Static files and `io` read the disk on the worker thread. On network file systems or slow disks this stalls other tasks on that thread. If it shows up, the fix is the SQLite approach (a file I/O thread) or `io_uring`. Measure before building either.
- **Lazy bodies change the request loop.** They are the riskiest runtime change in the plan: pipelining, keep-alive, chunked bodies and `100-continue` interact. Milestone 11 has the stress run as a gate, and adds a fuzz test of the parser with random splits of pipelined requests.
- **Metric cardinality.** Route patterns bound it, but application metrics with user-provided label values can explode. There is a cap of 1000 per metric (4.7), with folding into `other` and a warning.
- **Dashboard exposure.**
  - Basic credentials over plain HTTP can be read by anyone on the path. The warning in 4.8 and the documentation say to serve it over TLS or keep it on an internal port.
  - A later option is `server.metrics(address="127.0.0.1:9100")`, a separate listener just for metrics. It is a cheap addition if wanted.
- **Time zones.**
  - Systems without `/usr/share/zoneinfo` (Windows, slim containers) need decision 2. `FROM scratch` images need `tzdata` copied in, which the docs will say.
  - Zone rules change several times a year. Reading the system's files keeps them current; bundling would not.
- **Template escaping.** Contextual escaping without a full HTML parser can miss unusual markup, such as unquoted attributes or `<script>` written in odd case. The load-time tokenizer must handle these, and the escaping suite tests them. When unsure, it refuses at load time rather than guessing.
- **SQLite linking.** A system library ties binaries to the host's `libsqlite3` version. Old distributions (before 3.35) lack `RETURNING`. The module checks `sqlite3_libversion_number()` at open, and refuses below 3.35 with a clear message.
- **Several providers and the scheme rename** (Milestone 21) touch the OpenAPI goldens and the compiler's `SIGN_IN_SCHEME`. Keep the `googleSession` alias for one release.
- **The size of this plan.** At about 60 days it will not be built at once. Each milestone stands alone and is useful by itself, and the order can change (decision 15).
- **Not verified word for word**, to check before the milestone that relies on each:
  - The `RateLimit` draft's current header syntax (Milestone 14); it has changed between drafts. (Checked for Milestone 14: draft 12 uses `a` and `w`.)
  - OTLP/HTTP JSON's encoding rules: hex trace IDs and integer fields as strings (Milestone 10).
  - Microsoft's multi-tenant issuer validation (Milestone 21).
  - GitHub's PKCE support for OAuth apps (Milestone 22).
  - The Prometheus format's escaping of label values and `# HELP` text (Milestone 8).
  - OWASP's current PBKDF2-SHA256 iteration count (Milestone 9).

## 8. Sources

- Fetch standard, CORS protocol: https://fetch.spec.whatwg.org/#http-cors-protocol
- RFC 9110, HTTP Semantics (conditional requests, section 13; range requests, section 14; content codings, section 8.4): https://www.rfc-editor.org/rfc/rfc9110
- RFC 9111, HTTP Caching: https://www.rfc-editor.org/rfc/rfc9111
- RFC 1952, gzip: https://www.rfc-editor.org/rfc/rfc1952
- RFC 7932, Brotli: https://www.rfc-editor.org/rfc/rfc7932
- RFC 3339, Date and Time on the Internet: https://www.rfc-editor.org/rfc/rfc3339
- RFC 8536, the Time Zone Information Format (TZif): https://www.rfc-editor.org/rfc/rfc8536
- RFC 9562, UUIDs: https://www.rfc-editor.org/rfc/rfc9562
- RFC 7578, `multipart/form-data`: https://www.rfc-editor.org/rfc/rfc7578
- RFC 7617, HTTP Basic authentication: https://www.rfc-editor.org/rfc/rfc7617
- Prometheus exposition formats: https://prometheus.io/docs/instrumenting/exposition_formats/
- OpenTelemetry Protocol (OTLP): https://opentelemetry.io/docs/specs/otlp/
- OpenTelemetry HTTP semantic conventions: https://opentelemetry.io/docs/specs/semconv/http/http-spans/
- W3C Trace Context: https://www.w3.org/TR/trace-context/
- HTML standard, Server-Sent Events: https://html.spec.whatwg.org/multipage/server-sent-events.html
- IETF draft, RateLimit header fields for HTTP: https://datatracker.ietf.org/doc/draft-ietf-httpapi-ratelimit-headers/
- OpenID Connect Discovery 1.0: https://openid.net/specs/openid-connect-discovery-1_0.html
- GitHub, authorizing OAuth apps: https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps
- Go `net/http` `CrossOriginProtection`: https://pkg.go.dev/net/http#CrossOriginProtection
- OWASP CSRF prevention cheat sheet: https://cheatsheetseries.owasp.org/cheatsheets/Cross-Site_Request_Forgery_Prevention_Cheat_Sheet.html
- OWASP password storage cheat sheet: https://cheatsheetseries.owasp.org/cheatsheets/Password_Storage_Cheat_Sheet.html
- Kubernetes liveness, readiness and startup probes: https://kubernetes.io/docs/tasks/configure-pod-container/configure-liveness-readiness-startup-probes/
- The Twelve-Factor App, config: https://12factor.net/config
- SQLite C interface: https://www.sqlite.org/c3ref/intro.html
- Starlette middleware (CORS, GZip): https://www.starlette.io/middleware/
- Express `serve-static`: https://expressjs.com/en/resources/middleware/serve-static.html
