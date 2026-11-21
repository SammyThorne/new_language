# 05 — Standard library

The standard library is what makes Viper a backend language and not a general-purpose one. Its API is written in Viper (`stdlib/*.vi`). What sits behind the API moves over time from the Rust runtime into Viper itself.

## Layers

| Module | Purpose | Milestone | Implemented in (prototype) |
|---|---|---|---|
| prelude | `print` (with `sep`, `end` and `flush`), `len`, `range`, `str`, `int`, `float`, `list`, `ord`, `chr`, `reversed`, `min`, `max`, `sorted`, `panic`, built-in types and traits | M1–M4 | Compiler built-ins plus runtime |
| `io`, `os` | Files, standard streams, environment variables, arguments | M5 | Viper wrappers over runtime |
| `time` | Clocks, durations, `sleep` | M6 | Viper wrappers over runtime |
| `datetime` | Instants, dates, durations, time zones, RFC 3339 | docs/14, milestone 5 | Viper, over runtime parsing, formatting and TZif reading |
| `uuid` | Random and time-ordered UUIDs, parsing | docs/14, milestone 6 | Viper, over runtime generation and parsing |
| `config` | Typed settings from environment variables and `.env` files; secrets that never print | docs/14, milestone 7 | Viper, over a runtime `.env` parser and a lenient mode of the JSON decoder |
| `sync` | `Channel`, `Mutex`, `Atomic`, `spawn` support | M6 | Runtime |
| `json` | Dynamic values, typed encode and decode | M7 | Runtime scanner and writer; compiler-derived per-type code |
| `net` | TCP listener and stream | M8 | Runtime |
| `http` | Server, router, request, response, middleware, WebSockets | M8–M9 | Runtime protocol code; Viper API layer |
| `log` | Levelled, structured logging to standard error | M8 | Viper |
| `testing` | Assertions and the `@test` runner | post-prototype | Viper |
| `re` | Regular expressions | M12 | Viper API over the `regex-lite` crate |
| `postgres` | PostgreSQL client with a connection pool | M12 | Runtime protocol code; Viper API layer |
| `crypto` | Secure random values, SHA-256, HMAC, constant-time comparison, base64, RS256 checks | post-prototype (D101) | Viper API over the `ring` crate |
| `auth` | Sign-in with Google: login, callback and logout routes, a signed session cookie, route protection | post-prototype (D102) | Viper, on `http` and `crypto` |

Not in the prototype: HTTP client, TLS, database drivers, templating, regular expressions, cryptography beyond what the WebSocket handshake needs. An HTTP client and a PostgreSQL driver are the first two additions afterwards, since a backend without them is a toy. Milestone 12 added the client, TLS (server and client, through `rustls`), the PostgreSQL driver and regular expressions. The `crypto` and `auth` modules came with Google sign-in (docs/13, D101 and D102).

## `io` and `os` (M5)

```python
import io
import os

def main():
    for path in os.args()[1:]:
        match io.read_lines(path):
            case Ok(lines):
                print(path, len(lines))
            case Err(error):
                io.eprint(error.message())
                os.exit(1)
    with io.open("log.txt", "a").unwrap() as log:
        log.write("done\n").unwrap()
```

| Module | Functions and types |
|---|---|
| `io` | `read_file(path) -> Result[str, IoError]`, `read_lines`, `write_file(path, text)`, `append_file`, `exists`, `remove`, `list_dir`, `read_line() -> str?` (standard input), `eprint(text)` (standard error), `open(path, mode="r") -> Result[File, IoError]`; `File` has `read`, `read_line`, `lines`, `write`, `close`, and closes itself at the end of a `with` block; `IoError` implements `Error` |
| `os` | `args() -> list[str]`, `getenv(name) -> str?`, `environ() -> dict[str, str]` (every variable whose name and value are UTF-8), `setenv(name, value)` (at startup, before starting tasks: the environment is not safe to change while other threads read it), `exit(code)`, `cwd()` |

`input(prompt)` is also built in, returning `str?` (`None` at the end of input).

## Logging (docs/14)

```python
import log

log.info("listening", {"port": "8080"})
log.warn("slow query", {"ms": f"{ms:.1f}", "table": "orders"})

jobs = log.with_fields({"component": "billing"})       # a Logger, shareable between tasks
jobs.error("charge failed", {"customer": customer_id})
```

```
2026-11-20T09:30:00.123Z INFO  listening port=8080
2026-11-20T09:30:01.456Z WARN  slow query request_id=019aa3f2-5c41-7d0e-9b2a-4e8f1c6d3a70 ms=812.4 table=orders
```

| Item | Shape |
|---|---|
| `log.debug/info/warn/error(message, fields={})` | One line to standard error. `fields` (`dict[str, str]`) follow the message as `name=value`, in logfmt's form: a value with a space, `=`, `"` or a control character is quoted with JSON's escapes |
| `log.set_level(level)` | `debug`, `info` (the default), `warn`, `error` or `off`; also the `LOG` environment variable, with the language's prefix |
| `log.set_format(format)` | `"text"` (the default) or `"json"`: one object per line, `{"time":..., "level":"info", "msg":..., "request_id":..., ...fields}`. Also the `LOG_FORMAT` environment variable, with the language's prefix. A field named `time`, `level`, `msg` or `request_id` is written as `_time` and so on |
| `log.with_fields(fields) -> log.Logger` | A logger that adds `fields` to every line; it has the same four methods and its own `with_fields`. Fields given to a call win over the logger's |

**The request ID.** While `http.request_log()` handles a request, every line logged by the handler, by middleware and by tasks the handler spawns carries the request's `request_id`, without passing it around: the runtime keeps it with the running task, and a new task starts with its parent's.

**Buffering.** While a server runs and standard error is not a terminal (a pipe to a log collector, or a file), lines are gathered and written in blocks of up to 16 KiB, at the latest when the server's accept loop next wakes (every 200 ms). `warn` and `error` lines are written at once, and everything is written when the program exits, panics or calls `io.eprint`. On a terminal every line appears as it is logged.

## Core types

Method lists are indicative; the prototype implements what the demo programs need, then fills in.

| Type | Operations |
|---|---|
| `str` | `len`, `+`, `==`, `in`, slicing, iteration, `*` with an `int`; `upper`, `lower`, `strip`, `lstrip`, `rstrip` (with an optional set of characters), `split`, `rsplit` (with an optional limit), `join`, `replace`, `find`, `rfind`, `index`, `count`, `starts_with`, `ends_with`, `partition`, `rpartition`, `split_lines`, `remove_prefix`, `remove_suffix`, `is_digit`, `is_alpha`, `is_alnum`, `is_space`, `is_upper`, `is_lower`, `title`, `capitalize`, `zfill`, `encode`; `int.parse(s)`, `float.parse(s)` return `Result`. The methods beyond the first group are written in the prelude (D92) |
| `list[T]` | Literal `[a, b]`, indexing, slicing, `len`, `in`, iteration, `+`, `*` with an `int`; `append`, `extend`, `insert`, `pop`, `remove`, `clear`, `sort`, `reverse`, `index`, `copy`, `count`; `del xs[i]`; `reversed(xs)` and `list(x)` make new lists |
| `dict[K, V]` | Literal `{k: v}`, `d[k]` (panics if missing), `d[k] = v`, `in`, `len`; `get(k) -> V?`, `get_or(k, default)`, `get_or_insert(k, default)`, `pop`, `pop_or(k, default)`, `update`, `copy`, `keys`, `values`, `items`; `del d[k]` (panics if missing) |
| `set[T]` | Literal `{a, b}` and comprehension `{x for x in xs}`, `set()` and `set(xs)`; `in`, `len`, iteration, `sorted`, `sum`, `min`, `max`, `==`; `add`, `discard`, `remove`, `pop`, `clear`, `copy`, `update`; `union`, `intersection`, `difference`, `symmetric_difference`, `is_subset`, `is_superset`, `is_disjoint`, and the operators `\|`, `&`, `-`, `^`. A JSON array (D93) |
| `T?` | `unwrap`, `unwrap_or`, `is_some`, `is_none`, `map` |
| `Result[T, E]` | `unwrap`, `unwrap_or`, `is_ok`, `is_err`, `map_err` |
| `bytes`, `bytearray` | As built (M9, D57): `bytes` is defined in the prelude. `text.encode()` makes one; `len(b)`, `b[i]` (an `int` from 0 to 255), `b.slice(start, end)`, `+`, `==`, hashing, `decode() -> str?`, `hex()`, and printing as `b'...'`. `bytearray` is not implemented |

## HTTP

### What a service looks like

```python
import http
import log
import time

@derive(Json)
@frozen
struct Todo:
    id: int
    title: str
    done: bool = False

@derive(Json)
@frozen
struct NewTodo:
    title: str

@frozen
struct App:
    todos: Mutex[dict[int, Todo]]
    next_id: Atomic[int]

impl App:
    @http.get("/todos")
    def list(self, limit: int = 50) -> list[Todo]:        # ?limit=10 is optional
        with self.todos.lock() as todos:
            return todos.values()[:limit]

    @http.post("/todos", status=201)
    def create(self, body: NewTodo) -> Todo:               # JSON body; 400 if malformed
        todo = Todo(id=self.next_id.increment(), title=body.title)
        with self.todos.lock() as todos:
            todos[todo.id] = todo
        return todo

    @http.get("/todos/{id}")
    def get(self, id: int) -> Result[Todo]:                # 400 if id is not a number
        with self.todos.lock() as todos:
            match todos.get(id):
                case Some(todo):
                    return todo
                case None:
                    return Err(http.Error(404, "todo not found"))

    @http.delete("/todos/{id}")
    def remove(self, id: int):                             # returns nothing: 204
        with self.todos.lock() as todos:
            todos.pop(id)

@http.get("/health")
def health() -> str:
    return "ok"

def request_log(req: http.Request, next: http.Handler) -> Result[http.Response]:
    start = time.now()
    resp = next(req)?
    log.info(f"{req.method} {req.path} {resp.status} {time.since(start)}")
    return resp

def main():
    app = App(todos=Mutex({}), next_id=Atomic(0))
    server = http.Server()
    server.use(request_log)
    server.mount(app)           # every annotated method of App, bound to this value
    server.mount(health)        # an annotated function
    server.listen(":8080")
```

What the compiler does with this, all at compile time:

- For each annotated handler it checks the signature against the path, then generates the code that takes `id` from the path, decodes the body into `NewTodo`, calls the handler, and encodes the result as JSON.
- `server.mount(app)` becomes one route registration per annotated method.
- It checks that `App` can be shared between the tasks that serve requests: it is `@frozen` and its fields are a `Mutex` and an `Atomic`.

There is no reflection and no scanning at startup.

### Route annotations

| Form | Meaning |
|---|---|
| `@http.get(path)`, `@http.post(path)`, `@http.put(path)`, `@http.patch(path)`, `@http.delete(path)` | On a function or a method: a route for that HTTP method and path. Paths support `{name}` segments and a trailing `{rest...}` wildcard |
| `status=201` | Optional argument: the status for a successful response. The default is 200, or 204 when the handler returns nothing |
| `tags=["todos"]`, `deprecated=True`, `errors=[404, 409]`, `security=["token"]` | Optional arguments for the API documentation (below): the operation's tags, whether it is deprecated, the error statuses it can return besides the `400` the compiler generates, and the security schemes it accepts (any one of them). The handler's docstring gives the summary (its first paragraph) and the description (the rest). They change nothing about how requests are served |
| `max_body=10485760` | Optional argument: the largest request body the route reads, in bytes, in place of the group's or the server's (see "Request size limits") |
| `@http.ws(path)` | A WebSocket endpoint (below) |
| `server.mount(value)` | Registers every annotated method of the value's type, bound to that value |
| `server.mount(function)` | Registers one annotated function |
| `server.mount(value, prefix="/api/v1")` | The same, under a path prefix |

These are compiler-known annotations. They are not general decorators and users cannot define new ones.

### How handler parameters are bound

Each parameter is matched against these rules in order, at compile time. A parameter that matches none is a compile error.

| # | Parameter | Bound from | If the request does not fit |
|---|---|---|---|
| 1 | `self` | The mounted value | |
| 2 | Type `http.Request` | The raw request | |
| 3 | Type `http.WebSocket` (only under `@http.ws`) | The connection | |
| 4 | Name equals a `{name}` in the path | That path segment, parsed as the parameter's type: `int`, `float`, `bool`, `str`, or a type with a string form (`JsonString`, such as `uuid.Uuid` and `datetime.Date`), read by its `from_json_string` | 400, with the type's own message |
| 5 | Type derives `Json`, or is a `list` or `dict` of such | The JSON request body. At most one per handler | 400, with the JSON path of the problem |
| 6 | `int`, `float`, `bool`, `str` or a type with a string form, whose name is not in the path | The query parameter of that name. Required unless the type is `T?` or the parameter has a default (only `None` for a type with a string form) | 400 |
| 7 | Type `http.Header` or `http.Header?` (M12) | The request header named after the parameter: `user_agent` is `User-Agent` | 400 if missing, unless `T?` |
| 8 | Type `http.Cookie` or `http.Cookie?` (M12) | The cookie with the parameter's name | 400 if missing, unless `T?` |
| 9 | Type `auth.User` or `auth.User?` (D104), matched before rule 5 although `auth.User` derives `Json` | The signed-in user. At most one per handler, and not under `@http.ws` | `auth.User` protects the route: signed out is a 401, or a redirect to sign in for a browser. `auth.User?` is `None` |
| 10 | Type `http.Form[T]`, with `T` deriving `Json` (docs/14, milestone 12) | The form's text fields decoded into `T`, as `.value` (see "Forms and uploads"). At most one per handler | 400 naming each missing or malformed field; 415 for a body that is not a form |
| 11 | Type `http.Upload`, `http.Upload?` or `list[http.Upload]` | The file, or files, sent in the form under the parameter's name | 400 if missing, unless `T?` or a list |

Rules 10 and 11 read the body as a form, so they cannot be combined with a JSON body parameter (rule 5), nor used on a `get` or `delete` route or a WebSocket endpoint: error E0343.

`http.Header` and `http.Cookie` are frozen structs with a `value: str` field and `Display`. Other request details are read from an `http.Request` parameter; `req.cookie(name)` reads a cookie.

### The client (M12)

```python
resp = http.get("http://inventory:8080/items?limit=5")?           # Result[http.ClientResponse, http.ClientError]
if resp.ok():
    items = http.decode_json[list[Item]](resp)?
resp = http.post("http://audit/log", json.encode(entry))?        # content type JSON by default
resp = http.fetch("PUT", url, body, {"Authorization": token})?    # any method, any headers
```

| Item | Shape |
|---|---|
| `http.get(url)`, `http.post(url, body, content_type="application/json")`, `http.fetch(method, url, body="", headers={})` | Send a request and wait for the whole response. A task waiting here parks; it does not hold a worker. The request asks for gzip and a gzip body is decoded (up to 1 GiB), with `Content-Encoding` and `Content-Length` removed from the headers; pass your own `Accept-Encoding` header to get the body as sent |
| `http.post_form(url, fields, headers={})` | `POST` the `(name, value)` pairs as a form body (`application/x-www-form-urlencoded`), as OAuth token endpoints expect |
| `http.ClientResponse` | `status`, `body: bytes`, `ok()`, `header(name) -> str?` (a header sent several times, such as `Set-Cookie`, is its values joined by newlines), `text() -> Result[str]`, `json() -> Result[json.Value]` |
| `http.decode_json[T](response)` | The body decoded into `T` |
| `timeout=` | `http.fetch(..., timeout=datetime.seconds(5))` bounds the whole exchange, from connecting to the last byte; 30 seconds by default |
| `http.ClientError` | No response: unreachable host, timeout (30 seconds unless the call sets one), connection closed early, an untrusted TLS certificate, or a URL that is not `http://` or `https://`. Returned from a handler with `?`, it becomes a 502 |

One request per connection, HTTP/1.1 (D77). `https://` URLs use TLS and check the server's certificate against the Mozilla root store built into the program, plus the certificates in the file named by `SSL_CERT_FILE` if it is set (D79).

### TLS (M12)

```python
server.listen_tls(":8443", "cert.pem", "key.pem").unwrap()      # or bind_tls(...) and serve()
```

`server.bind_tls(addr, cert_file, key_file)` and `server.listen_tls(...)` are `bind` and `listen` for HTTPS. The certificate file holds the PEM chain, the server's certificate first; the key file holds its PEM private key (PKCS#8, PKCS#1 or SEC1). TLS 1.2 and 1.3 through `rustls`; the server offers ALPN `http/1.1`. WebSockets on an HTTPS server are secure WebSockets with no other change. The handshake happens in the connection's own task, on its first read, so a slow client holds nothing else up (D79).

### What a handler may return

| Return type | Response |
|---|---|
| `http.Response` | As given |
| A `Json` type, such as `Todo`, `list[Todo]` or `dict[str, int]` | JSON body with the annotation's status |
| `str` | `text/plain` body |
| Nothing | 204 with no body |
| `Result[...]` of any of the above | `Ok` as above; `Err` through the error mapping below |

### Compile errors specific to routes

- A `{name}` in the path with no parameter of that name.
- A parameter that no binding rule accepts, for example an ordinary struct that does not derive `Json`.
- Two body parameters, or a body parameter on a `get` or `delete` route.
- A form or upload parameter with a JSON body parameter, on a `get` or `delete` route, or two `http.Form[T]` parameters (E0343).
- A path parameter whose type cannot be parsed from text.
- A return type that cannot be turned into a response.
- Two routes with the same method and path on one type. Duplicates across separate `mount` calls are reported when the server starts.
- Mounting a value whose type is not shareable between tasks.

### The layer underneath

Annotations are sugar over a call-style API, which stays public:

```python
def get_todo(req: http.Request) -> Result[http.Response]:
    id = req.param_int("id")?
    ...
    return http.json(todo)

server.get("/todos/{id}", get_todo)
```

For each annotated handler the compiler writes a function of exactly this shape and registers it in exactly this way. The call style is the right tool for routes decided at run time, for middleware, and for tests. Milestone 8 builds this layer first and the annotations on top of it.

### OpenAPI and the documentation page

One call serves an [OpenAPI 3.1](https://spec.openapis.org/oas/v3.1.0) description of the server's routes and a page to read and try them, as FastAPI does at `/docs` and Fastify's swagger plugins at `/documentation`:

```python
server.mount(App(...), prefix="/api")
server.docs("/docs", title="Todo API", version="1.0.0")
```

| Path | What it serves |
|---|---|
| `/docs` (and `/docs/`) | The API documentation page: the operations grouped by tag, with their parameters, bodies, responses and schemas, a filter, a "Models" list, and "Try it out", which sends a real request and shows the answer and the same request as a `curl` command |
| `/docs/openapi.json`, `/docs/openapi.yaml` | The OpenAPI document |
| `/docs/viewer.css`, `/docs/core.js`, `/docs/viewer.js` | The page's own files: part of the runtime, about 46 KB, nothing loaded from elsewhere |

**Where the document comes from.** The compiler writes each mounted route's operation from what it already checks: the path, query, header and cookie parameters with their types and defaults, the JSON body, the success status and response type, and the `400` it generates for bad input. Types that derive `Json` become schemas under `components.schemas`, exactly as the encoder writes them (`T?` is `["string", "null"]`, sized numbers have their `format` (and unsigned ones `minimum: 0`), enums are a string enum or a `oneOf` tagged by `type`). The handler's docstring gives the summary and description (Markdown), and the annotation's `tags=`, `deprecated=`, `errors=` and `security=` the rest. Methods of a mounted type are tagged with the type's name unless `tags=` says otherwise. Routes registered in call style are listed with their method, path and path parameters only. The `docs` routes themselves are left out, and WebSocket endpoints too unless `hide_ws=False`.

**Only when used.** The compiler writes the documentation only for programs that mention `.docs` or `.openapi_json`, and the page's files are only in executables that call `docs`. A server without documentation compiles to the same code as before.

```python
server.docs(
    "/docs",
    title="Todo API",
    version="1.0.0",
    description="The **todo** service.",                  # Markdown
    servers=["https://todo.example.com"],                 # where "Try it out" sends; by default the page's own server
    tags=[http.Tag("todos", "Reading and writing todos."), http.Tag("admin")],   # descriptions, and the order of the groups
    security={"token": http.bearer(format="JWT"), "key": http.api_key("X-API-Key")},
)

@http.delete("/todos/{id}", security=["token"], errors=[401, 404])
def remove(self, id: int): ...
```

**Security schemes** describe how clients authenticate: `http.bearer(format="", description="")` (`Authorization: Bearer ...`), `http.basic_auth(description="")`, and `http.api_key(name, cookie=False, description="")` (a header, or a cookie). A route names the schemes it accepts with `security=[...]`; a route that names a scheme `docs` was not given is a bug, and building the document panics with both names. A library can document a scheme on every page with `server.security_scheme(name, scheme)`, which `docs(security=...)` overrides; `auth`'s `install` documents its session cookie as `googleSession`, which protected routes name on their own (D104). The page has an "Authorize" box where a reader enters credentials, kept in the page's memory only, and "Try it out" adds them to its requests. The documentation does not check anything: authenticate requests in middleware, as with any route, and list `401` in `errors=`.

**Other arguments.** `ui=False` serves only the document; `try_it_out=False` makes the page read-only. `server.openapi_json(...)` takes the same arguments (except the path and the page's) and returns the document as text: for a test, a file, or a route of your own.

**What to know.**
- The docs routes are ordinary routes: middleware applies to them, so a server that authenticates everything authenticates its documentation too.
- The document is built on its first request and again after a route is added.
- The page needs no inline script or style; it is served with `Content-Security-Policy: default-src 'self'; connect-src *`, `ETag` and `Cache-Control: no-cache`.
- Two different types whose names would make the same schema name are a compile error (`E0341`).
- A generic struct cannot be a handler's body or return type directly; wrap it in a struct of its own.
- OAuth2 and OpenID Connect flows, file uploads and XML bodies are not described (a token pasted into "Authorize" works).

### API surface

| Item | Shape |
|---|---|
| `http.Server()` | Creates a server with an empty router |
| `server.mount(value_or_function, prefix="")` | Registers annotated routes (above) |
| `server.get/post/put/patch/delete/head/options(path, handler)` | Register one route in call style; `post`, `put`, `patch` and `delete` take `max_body=` |
| `server.route(method, path, handler, max_body=0)` | General form |
| `server.group(prefix)` | Returns a sub-router sharing a prefix and its own middleware |
| `server.use(middleware)` | Middleware has the type `def(http.Request, http.Handler) -> Result[http.Response]` |
| `http.request_log(skip=[], trace=False)` | Middleware that gives each request an ID and logs one line when it is answered: `GET /photos/7 200` with the fields `method`, `path` (without the query string), `status`, `ms`, `route` (the pattern of the route that took it, when one did) and, for a request from a connection, `client` (`req.client_ip()`); `error` for a 5xx, otherwise `info`. The ID is the client's `X-Request-Id` when it is 1 to 128 ASCII letters, digits, `.`, `_` and `-`, otherwise a new one; it is sent back in `X-Request-Id` (error responses included), kept as the request local `"request_id"`, and carried by log lines (see "Logging"). Paths in `skip` get neither, and nor do `server.health`'s routes. Add it first, so it times the other middleware too. A handler that panics gets no line: the panic is reported instead. With `trace=True`, it also turns tracing on from the environment (see "Traces"), and its lines carry `trace_id` and `span_id` |
| `http.cors(origins=[], allow=None, methods=[GET, HEAD, POST, PUT, PATCH, DELETE], headers=["Content-Type", "Authorization"], expose=[], credentials=False, max_age=600)` | Middleware for cross-origin requests from browsers. `origins` lists allowed origins exactly as browsers send them (`"https://app.example.com"`, no path) or is `["*"]`; `allow`, a `def(str) -> bool`, decides for the others. A preflight (`OPTIONS` with `Origin` and `Access-Control-Request-Method`) gets a 204 here, allowing `methods`, the requested headers in `headers` (`["*"]` for any) and caching for `max_age` seconds; a refused origin's preflight gets a 204 without them. Other requests from an allowed origin get `Access-Control-Allow-Origin` (the origin with `Vary: Origin`, or `*` for `["*"]` without credentials), `Access-Control-Expose-Headers` for `expose` and, with `credentials=True`, `Access-Control-Allow-Credentials`, on error responses too. Panics at setup for no origins, an origin with a path, or credentials with `*`. On a group (`api.use(http.cors(...))`) it covers that group's routes and their preflights |
| `http.compress(min_size=1024, level=6, types=["text/*", "application/json", "application/javascript", "image/svg+xml"])` | Middleware that gzips response bodies for clients whose `Accept-Encoding` allows `gzip` (q-values read): bodies of at least `min_size` bytes whose content type matches `types` (`"text/*"` for every `text/` type) and that have no `Content-Encoding`. A client that refuses uncompressed bodies (`identity;q=0` or `*;q=0`) gets gzip for any non-empty body. Leaves alone 1xx, 204, 206 and 304 responses, `Cache-Control: no-transform`, errors returned as `Err`, and `server.static` files (which use `.gz` files instead). Compressed responses get `Content-Encoding: gzip` and a weak `ETag`; every response that could be compressed gets `Vary: Accept-Encoding`. `level` is 0 (stored) to 9 (smallest); panics at setup outside that or for a negative `min_size` |
| `server.trusted_proxies` | `list[str]` of addresses and networks (`"10.0.0.0/8"`, `"::1"`) whose `X-Forwarded-For` header is believed, for `req.client_ip()`; empty by default. Set it before `bind`. It applies to every server in the process |
| `server.static(prefix, root, max_age=0, immutable=False, index="index.html", dotfiles=False, fallback="")` | Serves the files under the directory `root` at `prefix` (`GET` and `HEAD`), from the runtime, so binary files and large ones cost no `str` copies. The content type comes from the extension (about 60 known; text types with `; charset=utf-8`, others `application/octet-stream`), with `X-Content-Type-Options: nosniff`. Responses carry a strong `ETag` (size and modification time), `Last-Modified` and `Cache-Control: public, max-age=<max_age>` (`, immutable` when set), and answer `If-None-Match`, `If-Modified-Since`, `If-Match`, `If-Unmodified-Since` (304, 412) in RFC 9110's order. One `Range` gets a 206 (an unsatisfiable one a 416, several the whole file), `If-Range` honoured. `file.br` or `file.gz` beside a file is sent in its place, with `Content-Encoding` and `Vary: Accept-Encoding`, when the client accepts it and asks for no range. Files over 1 MiB are sent in 256 KiB pieces. A directory serves `index`, and one named without its trailing `/` is redirected (301). `fallback` is sent, with `Cache-Control: no-cache`, for paths with no file (single-page apps). `..`, an encoded `/` or `\`, NUL, empty segments, symbolic links leading out of `root` and dotfiles (unless `dotfiles=True`) are 404s. Panics at setup if `root` is not a readable directory. Routes without parameters are tried first; routes with parameters, in the order added. The files are not in the API documentation. Middleware applies; headers it adds are kept |
| `server.ws(path, handler)` | WebSocket endpoint in call style (below) |
| `server.docs(path="/docs", title, version, description, servers, tags, security, ui, try_it_out, hide_ws)` | Serves the API documentation page and the OpenAPI document (above) |
| `server.openapi_json(title, version, description, servers, tags, security, hide_ws)` | The OpenAPI document as JSON text |
| `http.Tag(name, description="")` | A tag's description, for `docs(tags=...)` |
| `http.bearer(...)`, `http.basic_auth(...)`, `http.api_key(name, cookie=False, ...)` | `http.SecurityScheme` values, for `docs(security=...)` |
| `server.security_scheme(name, scheme)` | A scheme documented on every page of the server, for libraries |
| `server.listen(addr)` | Blocks, serving until shutdown. Options: read and write timeouts, maximum header and body sizes, graceful shutdown on SIGTERM |
| `server.metrics(path="/metrics", dashboard=True, user=None, password=None, public=False)` | Serves the metrics in the Prometheus text format, and a dashboard at `path + "/dashboard"`, behind HTTP Basic authentication; see "Metrics and health" |
| `server.health(live="/healthz", ready="/readyz", checks={})` | Liveness and readiness routes for a load balancer; see "Metrics and health" |
| `server.drain_delay` | Seconds to go on serving after SIGTERM, with readiness failing, before closing; 0 by default |
| `server.max_files`, `server.max_fields`, `server.max_part_headers`, `server.upload_dir` | Limits on a form: 10 files and 1000 text fields (else 413), 8 KiB of headers in one multipart part (else 400); the directory where files over 1 MiB wait while the request is handled (`""`, the default, for the system's temporary directory). See "Forms and uploads" |
| `http.Handler` | `def(http.Request) -> Result[http.Response]` |
| `http.Request` | `method`, `path`, `param(name) -> str`, `param_int(name) -> Result[int]` (fails with a 400 `http.Error`), `query(name) -> str?`, `query_string() -> str` (as sent, without `?`), `header(name) -> str?`, `cookie(name) -> str?` (`%XX` decoded), `body() -> bytes` (any bytes, for binary uploads), `text() -> Result[str]`, `json[T]() -> Result[T]`, `remote_addr() -> str` (the connection's other end, `host:port`), `client_ip() -> str` (the client's address without a port: through `server.trusted_proxies`, the rightmost address in `X-Forwarded-For` that is not one of them, so a client cannot choose it; otherwise the connection's; `""` for a test request); `form() -> Result[http.FormData]` (see "Forms and uploads"); `set_local(name, value)` and `local(name) -> str?` keep values with the request for the handlers after a middleware (docs/13); `route() -> str` is the pattern of the route that took the request (`/todos/{id}`), `""` before routing or when none did |
| `http.Response` | `status`, `headers`, `body`; built with `http.text`, `http.json`, `http.binary(data, content_type="application/octet-stream", status=200)` (a body of any bytes), `http.error`, `http.redirect`, `http.no_content`; `body_bytes()` is the body as sent, for tests. `with_header(name, value)` sets a header; `add_header(name, value)` adds another value, sent as its own line (in `headers`, the values are joined by newlines) |
| `response.set_cookie(name, value, max_age=-1, path="/", http_only=True, secure=False, same_site="Lax")` | Adds a `Set-Cookie` header; any number per response. Characters a cookie cannot hold, and `%`, are sent as `%XX`, which `req.cookie` decodes. `__Host-` and `__Secure-` cookies, and `same_site="None"`, are always `Secure`. An invalid name, path or `same_site` is a bug and panics (a 500). `delete_cookie(name, path="/")` expires one; `response.cookie(name) -> str?` reads one back, for tests |
| `http.url_encode(text)`, `http.url_decode(text, plus_is_space=True) -> str?`, `http.form_encode([(name, value), ...])` | Percent-encoding: everything but ASCII letters, digits and `-._~` as `%XX` of its UTF-8 bytes. `form_encode` makes a query string or an `application/x-www-form-urlencoded` body |
| `http.test_request(method, path, body="", headers={}, form={}, files={})` | A request without a connection, for `server.handle(...)` in tests. A query string on `path` is what `query` reads; `headers` are what `header` and `cookie` read. `form` (`dict[str, str]`) sends text fields as an urlencoded body; with `files` (`dict[str, http.Upload]`, each made by `http.test_file(filename, content, content_type="application/octet-stream")`), a multipart form |
| `http.Error` | An `Error` carrying a status code |

### Request size limits (docs/14, milestone 11)

```python
server.max_body = 1_048_576                                # 1 MiB for every route; the default is 8 MiB
server.post("/photos", upload, max_body=50 * 1_048_576)   # 50 MiB for this one
admin = server.group("/admin")
admin.max_body = 10_000                                    # for the group's routes

@http.post("/avatars", max_body=2_097_152)
def avatar(req: http.Request) -> Result[str, http.HttpError]: ...
```

- **Which limit.** A route's `max_body`, else its group's (a nested group starts with its parent's), else `server.max_body`. A body larger than it gets **413**, with `the request body is larger than N bytes`.
- **Bodies are read when the handler asks.** A body of up to 64 KiB (and `server.max_body`) is read with the request's head, as nearly every API request's is. A larger one, by `Content-Length` or chunked, is read only when the handler calls `req.text()`, `req.json()`, `req.body()` or `http.body[T]`, against the route's limit: a `Content-Length` past it is refused before any of it is read. So a route that allows large uploads does not make every route accept them.
- **The 413 is final.** `req.text()` and the others return it as an error, and `req.body()`, which has no error to return, gives empty bytes: either way the client is answered 413, whatever the handler returns.
- **`Expect: 100-continue`** is answered with `100 Continue` only when the handler first reads the body, so a client that asks first never sends an upload that is refused, or that the handler does not want.
- **A body the handler does not read** is read and dropped after the response when no more than 1 MiB of it is left, so the connection can serve its next request; past that, the response says `Connection: close` and the connection closes. A closing connection goes on reading what arrives for up to two seconds, so the client sees the response rather than a reset.
- **Time.** A body read later must keep arriving: 30 seconds without any of it is a 408 (an upload may take longer in all).
- **Heads.** At most `server.max_headers` headers (100; else **431**), a request target, path and query string, of at most `server.max_url` bytes (8 KiB; else **414**), and a head of at most 64 KiB (431).
- **Test requests** (`server.handle(http.test_request(...))`) check a route's or group's `max_body` in `req.text()`, so a test can see the 413; `server.max_body` and the head limits apply only to requests from a connection.

### Forms and uploads (docs/14, milestone 12)

```python
@derive(Json)
struct Profile:
    name: str
    age: int
    newsletter: bool = False                     # an unchecked box sends nothing

@http.post("/profile", max_body=10_485_760)
def save_profile(form: http.Form[Profile], photo: http.Upload?) -> Result[str, http.HttpError]:
    match photo:
        case Some(file):
            file.save(f"photos/{form.value.name}.jpg")?     # choose the path: `filename` is the client's
        case None:
            pass
    return f"saved {form.value.name}"

def handler(req: http.Request) -> Result[http.Response]:   # or by hand
    form = req.form()?
    tags = form.all("tag")                       # list[str]
    for doc in form.files("docs"):               # list[http.Upload]
        print(doc.filename, doc.size)
    return http.text(form.get("title").unwrap_or("untitled"))
```

- **What is read.** `req.form()` reads `application/x-www-form-urlencoded` and `multipart/form-data` (RFC 7578) bodies; any other content type is a **415**. The body is read whole first, under the route's `max_body` (see "Request size limits"), and then split, in the runtime.
- **`http.FormData`.** `get(name) -> str?` and `file(name) -> http.Upload?` give the first text field or file of a name; `all(name) -> list[str]` and `files(name) -> list[http.Upload]` give every one, in order; `names()` lists the text fields. `req.form()` parses the body each time it is called, so keep the result.
- **`http.Upload`.** `filename`, `content_type` and `size`; `bytes() -> Result[bytes]`, `text() -> Result[str]` (a 400 if it is not UTF-8) and `save(path) -> Result[None]`. `filename` is the base name the client sent, without any directory, `/`, `\` or control character (`../../etc/passwd` is `passwd`, `C:\photos\me.jpg` is `me.jpg`), and may be `""`; neither it nor `content_type` is checked against the contents. A file sent as `filename*` (RFC 5987) gets that name. A file input left empty, which browsers send as a part with an empty file name and no data, is no file.
- **Large files.** A file over 1 MiB is written to a temporary file (readable only by the program's user) in `server.upload_dir`, the system's temporary directory by default, and removed when the request ends. `save` moves it rather than copying it when it can. Read or save an upload in the handler: after the request, `bytes()` on a large one is a 500.
- **`http.Form[T]`** decodes the text fields into `T` as `config.load` reads environment variables: numbers and booleans from their text (`on`, which a checked box sends, is `True`), a field sent several times as a list, and a list field sent once may be comma-separated. Every missing or malformed field is named in one 400: ``invalid form: field `age` must be an integer, got "x"; field `name` is required``. Files are not part of `T`: take them as `http.Upload` parameters.
- **Limits.** More than `server.max_files` files (10) or `server.max_fields` text fields (1000) is a **413**; a multipart part with more than `server.max_part_headers` bytes of headers (8 KiB), or a malformed body, is a **400** that says what is wrong.
- **Lenient where browsers and libraries differ.** Lines may end in a lone LF as well as CRLF, a boundary line may have trailing spaces, a preamble before the first boundary is ignored, and a quoted file name may hold unescaped backslashes (a Windows path, as some browsers send). A line that only starts like the boundary (`--boundaryX`) is data.
- **The API documentation** shows such routes with a `multipart/form-data` request body (and `application/x-www-form-urlencoded` when there are no files): `T`'s schema for the text fields and `{"type": "string", "format": "binary"}` for each file, an array of them for a list. The page's "Try it out" has a file input for each file and sends the form as the browser would.
- **Test requests.** `http.test_request("POST", "/profile", form={"name": "Ada", "age": "36"}, files={"photo": http.test_file("me.jpg", data, content_type="image/jpeg")})` sends a form without a connection; a multipart body can also be given whole with `body=` and its `Content-Type` header. Test requests keep every file in memory, and use `max_files` and the other limits once the server has composed its routes.

### Error mapping

When a handler returns `Err`, in either style:

- An `http.Error` becomes a response with its status and a JSON body `{"error": message}`.
- A JSON decode error from `req.json` becomes 400.
- Any other error becomes 500, is logged with its message, and the client sees a generic body.

The mapping is overridable with `server.on_error(handler)`.

### Implementation

As built in M8 (D49, D50), and since M10 with each connection on a fiber rather than a thread (D61, D65): the request parser, connection handling, limits and timeouts are in the runtime, written by hand rather than with `httparse`; the router, `Request`, `Response`, middleware and error mapping are written in Viper in `stdlib/http.lang`, with routes tried in order rather than through a radix tree. `http.Error(status, message)` is a function returning the struct `http.HttpError` (D51). `server.bind(addr)` and `server.serve()` split `listen` in two, so a program can learn its port (`":0"` picks a free one); `server.handle(req)` with `http.test_request(...)` answers a request without a socket, for tests.

The original plan for the prototype, inside the runtime:

- One accept loop. Each connection is handled by a task (stage 1: thread).
- Request parsing with the `httparse` crate into a reusable per-connection buffer. Headers are exposed to Viper lazily: a header is turned into a Viper string only when asked for.
- Routing with a radix tree (the `matchit` crate or equivalent). Route handlers are stored as handles ([04](04-runtime-and-memory.md#the-rust-heap-rule)).
- HTTP/1.1 keep-alive, `Content-Length` and chunked request bodies, `Expect: 100-continue`, `HEAD` handling, automatic `Date` and `Content-Length` response headers.
- Limits on header size, body size and idle time, on by default.
- Responses are serialised into the connection's write buffer and flushed with one system call where possible.

After the prototype: the router and the request/response types move into Viper; the byte-level parser may stay native indefinitely. TLS was added in the runtime with `rustls` in M12. HTTP/2 is considered only if there is demand, since backends usually sit behind a proxy that speaks HTTP/1.1 upstream.

## WebSockets

(M9)

```python
@frozen
struct Hub:
    clients: Mutex[dict[int, http.WebSocket]]

impl Hub:
    @http.ws("/chat")
    def handle(self, ws: http.WebSocket):
        with self.clients.lock() as clients:
            clients[ws.id] = ws

        for msg in ws:                          # ends when the peer closes
            match msg:
                case Text(body):
                    self.broadcast(body)
                case Binary(_):
                    ws.close(1003, "text only")

        with self.clients.lock() as clients:
            clients.pop(ws.id)

    def broadcast(self, body: str):
        with self.clients.lock() as clients:
            for client in clients.values():
                _ = client.send(body)           # ignore peers that have gone away

def main():
    hub = Hub(clients=Mutex({}))
    server = http.Server()
    server.mount(hub)
    server.listen(":8080")
```

| Item | Shape |
|---|---|
| `@http.ws(path)` | On a function or method that takes an `http.WebSocket`. It runs in its own task for the life of the connection. Path parameters bind as they do for HTTP routes |
| `server.ws(path, handler)` | The call-style form; `handler` has type `def(http.WebSocket)` |
| `ws.recv() -> http.WsMessage?` | Waits for the next message; `None` after close. `for msg in ws` is the same thing |
| `http.WsMessage` | `enum`: `Text(body: str)`, `Binary(data: bytes)` |
| `ws.send(text) -> Result[None, http.WsClosed]`, `ws.send_bytes(data)`, `http.send_json(ws, value)` | Safe to call from any task; sends are serialised per connection. `send_json` is a function rather than a method because methods cannot have type parameters yet (D33) |
| `ws.close(code=1000, reason="")` | Starts the closing handshake |
| `ws.id`, `ws.path`, `ws.param(name)`, `ws.query(name)`, `ws.header(name)`, `ws.remote_addr()` | Connection id and the upgrade request. As built, these replace a `ws.request` field, so that a `WebSocket` stays shareable (D58) |
| `server.max_message`, `server.ping_interval` | The largest message accepted (16 MiB by default) and the keep-alive ping interval (30 seconds) |

Implementation, as built in M9 (D58): the handshake, framing and control frames are in the runtime (`crates/lang_rt/src/ws.rs`); `WebSocket`, `WsMessage` and `upgrade` are in `stdlib/http.lang`. A WebSocket endpoint is an ordinary `GET` route whose handler calls `http.upgrade(req, handler)`, so middleware runs around it. The planned `authorize=` hook is not there: middleware or a check before calling `upgrade` does the same job in the call style.

The plan:

- The upgrade handshake (header validation, `Sec-WebSocket-Accept`) happens in the HTTP layer; an optional `server.ws(path, handler, authorize=fn)` hook can reject before upgrading.
- Framing follows RFC 6455: masking, fragmentation and reassembly, control frames interleaved with data frames, UTF-8 validation of text messages, size limits.
- Ping, pong and close frames are handled by the runtime. A configurable keep-alive ping detects dead peers.
- Stage 1 uses one task (thread) per connection, which caps practical connection counts in the low thousands. Stage 2 fibers remove that cap without API changes.
- Extensions (`permessage-deflate`) are not in the prototype.
- Conformance is checked with the Autobahn test suite ([07](07-testing-and-performance.md#protocol-conformance)).

## Regular expressions (M12)

```python
import re

word = re.compile("\\b(?P<first>\\w)(\\w*)").unwrap()
print(word.replace_all("hello there", "${first}."))      # h. t.
for m in word.find_all("one two"):
    print(m.start, m.text)
```

| Item | Shape |
|---|---|
| `re.compile(pattern) -> Result[re.Regex, re.RegexError]` | Perl-like syntax without look-around or backreferences; matching time is linear in the text's length (D76) |
| `regex.is_match(text)`, `regex.find(text, start=0) -> re.Match?`, `regex.find_all(text) -> list[re.Match]`, `regex.split(text) -> list[str]`, `regex.replace(text, rep)`, `regex.replace_all(text, rep)` | `$1` and `${name}` in a replacement insert groups |
| `re.Match` | `text`, `start`, `end` (byte offsets), `group(i) -> str?`, `named(name) -> str?` |
| `re.Regex` | Frozen: one compiled pattern can be shared by every task |

## Cryptography (D101)

```python
import crypto

session_id = crypto.token()                                  # 43 URL-safe characters
digest = crypto.sha256("abc".encode()).hex()                 # ba7816bf...
mac = crypto.hmac_sha256(secret.encode(), body.encode())
if not crypto.equal(mac, crypto.base64url_decode(sent).unwrap_or("".encode())):
    return http.Response(401)
```

| Item | Shape |
|---|---|
| `crypto.random_bytes(n) -> bytes` | `n` bytes from the operating system's secure random source; panics if `n` is negative |
| `crypto.token(n_bytes=32) -> str` | `n_bytes` random bytes as base64url without padding: session identifiers, CSRF tokens, PKCE verifiers |
| `crypto.sha256(data) -> bytes`, `crypto.hmac_sha256(key, data) -> bytes` | 32-byte digests; `.hex()` writes one as 64 hex digits |
| `crypto.equal(a, b) -> bool` | Compares in a time that depends only on the lengths. Use it for secrets and signatures, never `==` |
| `crypto.base64_encode(data) -> str`, `crypto.base64_decode(text) -> bytes?` | RFC 4648 standard alphabet; encoding pads with `=`, decoding accepts it with or without padding |
| `crypto.base64url_encode(data) -> str`, `crypto.base64url_decode(text) -> bytes?` | The URL-safe alphabet (`-`, `_`) without padding, as JSON Web Tokens use; padding is accepted when decoding |
| `crypto.verify_rs256(modulus, exponent, message, signature) -> bool` | RSASSA-PKCS1-v1_5 with SHA-256 (JWT `RS256`) by the public key with this big-endian modulus and exponent (a JSON Web Key's `n` and `e`, decoded); keys of 2048 to 8192 bits, anything else is `False` |

Data is `bytes`; `text.encode()` makes some, and passing a `str` instead is a compile error whose help says so. Decoding rejects any text no encoder writes (wrong characters, wrong padding, non-zero left-over bits), so each value has one encoding. There is no encryption: sessions are signed, not sealed (docs/13, decision 3). Everything is on `ring`, which the runtime already used for TLS.

## Signing in with Google (D102)

```python
import http
import auth
import os

def notes(req: http.Request) -> Result[http.Response]:
    user = auth.user(req).unwrap()                    # `auth.User?`; always `Some` behind `protect`
    return http.text(f"notes of {user.name}")

def main():
    server = http.Server()
    google = auth.google("secrets/client_secret.json", secret=os.getenv("SESSION_SECRET")).unwrap()
    google.install(server)
    server.get("/notes", auth.protect(notes))
    admin = server.group("/admin")
    admin.use(auth.required)                          # every route of the group
    server.listen(":8080").unwrap()
```

`auth.google(...)` reads the OAuth client file the Google Cloud console makes (its `web` or `installed` section: the client ID, the secret and the redirect URIs) and checks the setup, returning an `AuthError` that says what to change. `install(server)` adds:

| Route | What it does |
|---|---|
| `GET /auth/google/login?next=/notes` | Sets a signed, ten-minute flow cookie holding the `state`, `nonce`, PKCE verifier and `next`, and redirects to Google |
| `GET` the redirect URI's path | Checks the flow cookie and `state`, exchanges the code (with the PKCE verifier), checks the ID token's claims, applies the policies and `on_login`, sets the session cookie and redirects to `next` |
| `POST /auth/logout` | Refused (403) if `Origin` names another site; otherwise deletes the session cookie and redirects (303) to `/` |
| middleware | Reads and verifies the session cookie on every request |

| Item | Shape |
|---|---|
| `auth.google(credentials="", secret=None, redirect_uri="", scopes=["openid", "email", "profile"], allowed_domains=[], allowed_emails=[], require_verified_email=True, session_days=7, login_path=..., logout_path=..., after_logout="/", on_login=None) -> Result[auth.Provider, auth.AuthError]` | `credentials=""` reads `GOOGLE_CLIENT_SECRETS` (a path), or `GOOGLE_CLIENT_ID` and `GOOGLE_CLIENT_SECRET`; `secret=None` reads `SESSION_SECRET` |
| `provider.install(server)` | The routes and the middleware above |
| `provider.login_url(next="/") -> str` | The login route's URL, for links |
| `provider.logout_response(location="/") -> http.Response` | For an app's own logout route |
| `provider.test_cookie(user, expires_in=None) -> str` | A `Cookie` header value signing a test request in |
| `auth.google_for_tests(secret=..., endpoints=None, ...) -> auth.Provider` | A provider without a credentials file; `endpoints=auth.Endpoints(...)` points it at a fake Google; `keys=""` there skips the signature check, for a fake that cannot sign |
| `auth.user(req) -> auth.User?` | The signed-in user |
| `@auth.required` | On an `@http` route: refuses signed-out requests (D104) |
| `auth.required` | Middleware refusing signed-out requests |
| `auth.protect(handler) -> http.Handler` | One handler, protected |
| `auth.User` | `id` (Google's `sub`: key records on it), `email`, `email_verified`, `name`, `given_name`, `family_name`, `picture`, `domain` (the Workspace domain, or `""`), `provider` |
| `auth.Tokens`, `auth.LoginHook` | `on_login(user, tokens) -> Result[auth.User, http.HttpError]` may change the user (not its `id`) or refuse with an error's status; the module keeps no tokens |

Routes that `server.mount` serves are protected by the compiler (D104). `@auth.required` protects a route, and so does a parameter of type `auth.User`, which also gets the user; an `auth.User?` parameter is `None` for a signed-out request, which is not refused:

```python
@frozen
struct Notes:
    db: Pool

impl Notes:
    @http.get("/notes")
    def list(self, user: auth.User) -> list[Note]:           # protected
        return self.db.notes_of(user.id)

    @auth.required
    @http.get("/stats")
    def stats(self) -> Stats:                                 # protected, without the user
        return self.db.stats()

    @http.get("/")
    def home(self, user: auth.User?) -> str:                  # open
        return f"Hello, {user.unwrap().name}" if user != None else "Hello, guest"
```

The generated code checks the sign-in before it reads any parameter, so a signed-out request with a bad body gets the 401, not a 400. In the API documentation (docs/12), a protected route needs the security scheme `googleSession`, the session cookie, which `install` documents, and lists a 401 response. `@auth.required` on a function that is not a route, with arguments, or an `auth.User` parameter under `@http.ws` or twice, is error E0342.

A signed-out request to a protected route is refused in one of two ways. A browser opening a page (a `GET` or `HEAD` whose `Accept` names `text/html`) is redirected to the login route, with `next` set to the page and its query. Anything else gets `401 {"error": "sign-in required"}`. `next` must be a path on the site; anything else becomes `/`. A protected route on a server without a provider is a 500, with the reason logged.

The session cookie is `session`, or `__Host-session` when the redirect URI is `https`, and it is `HttpOnly` and `SameSite=Lax`. Its value is the JSON of the user and its times, signed with HMAC-SHA256. The key is derived from the secret, so the cookie holds no tokens and needs no database. Changing `SESSION_SECRET` signs everyone out. Without a secret, `http://localhost` development gets a key made for the run, with a warning, and an `https` setup is an error. The callback refuses an expired or missing flow cookie, a wrong `state`, an `error` from Google, a refused code, and an ID token whose header is not RS256 or whose issuer, audience, times (60 seconds of leeway) or nonce are wrong. Each refusal shows a short page with a link to try again, and the reason for a refused token is logged as a warning. The token's RS256 signature is checked against Google's published keys (D103). The provider caches them for the `max-age` Google sends, from an hour to a day, fetches them again for a key it does not have, and holds a lock while it fetches, so sign-ins arriving together fetch once.

A test signs a request in without Google:

```python
google = auth.google_for_tests()
google.install(server)
cookie = google.test_cookie(auth.User(id="1", email="a@example.com", email_verified=True))
response = server.handle(http.test_request("GET", "/notes", headers={"Cookie": cookie}))
```

What `on_login` and `auth.protect` are given is called by the tasks serving requests, so it must be shareable, as route handlers must.

### Setting it up

[`examples/google_login`](../examples/google_login/README.md) is a small app to start from, with its tests.

- **The client.** In the Google Cloud console's Credentials page, create an OAuth client ID of type "Web application" (set up the consent screen first if asked; an app in testing, with test users, needs no review). Download its JSON file. It holds the client secret: keep it out of version control (this repository ignores `client_secret*.json`), and in production prefer `GOOGLE_CLIENT_SECRETS`, or `GOOGLE_CLIENT_ID` and `GOOGLE_CLIENT_SECRET`, from the deployment's secrets.
- **The redirect URI.** Add `http://localhost:8080/auth/google/callback` to the client's authorized redirect URIs for development, and the public one, `https://example.com/auth/google/callback`, for production. Google compares them exactly. A file with one URI needs no `redirect_uri=`; with several, pass the one this server answers on. The callback route is served at that URI's path.
- **`SESSION_SECRET`.** At least 32 random bytes, such as `head -c 32 /dev/urandom | base64`, the same on every instance of the app. Changing it signs everyone out.
- **Behind a proxy.** The redirect URI is the address the browser sees, not the one the app listens on: a proxy that ends TLS in front of an app on `:8080` still needs `redirect_uri="https://example.com/auth/google/callback"`. The `https` URI makes the cookies `Secure` with the `__Host-` prefix, which browsers accept because they reach the site over `https`. The proxy must pass the `Host` header through, which `POST /auth/logout` compares with `Origin`.
- **Workspace accounts.** `allowed_domains=["example.com"]` admits only accounts of that Google Workspace domain, and sends Google a hint to show only those accounts when there is one domain.

## PostgreSQL (M12)

```python
import postgres

@derive(Json)
struct User:
    id: int
    name: str
    email: str?

db = postgres.connect("postgres://app:secret@localhost/app").unwrap()   # a pool; share it freely
db.execute("insert into users (name, email) values ($1, $2)", [name, None])?
users = postgres.query_as[User](db, "select id, name, email from users where id > $1", [str(after)])?
for row in db.query("select name, score from players")?:
    print(row.text("name")?, row.float("score")?)
with db.transaction()? as tx:            # rolled back unless committed
    tx.execute("update accounts set balance = balance - $1 where id = $2", [amount, a])?
    tx.execute("update accounts set balance = balance + $1 where id = $2", [amount, b])?
    tx.commit()?
```

| Item | Shape |
|---|---|
| `postgres.connect(url) -> Result[postgres.Db, postgres.DbError]` | `postgres://user:password@host:port/database?sslmode=...&pool_max=...`. Opens one connection to check the URL and credentials |
| `postgres.Db` | Frozen: one pool, shared by every task. `query(sql, args=[]) -> Result[list[Row]]`, `query_row(...) -> Result[Row?]`, `execute(...) -> Result[int]` (rows affected), `script(sql)` (several statements, no parameters), `transaction() -> Result[Db]`, `close()` |
| A transaction | A `Db` holding one connection: the same methods, plus `commit()`, `rollback()`, and `exit()` so that `with` rolls back if `commit` was not reached. A commit after a failed statement is an error (`25P02`) |
| `postgres.query_as[T](db, sql, args=[]) -> Result[list[T]]`, `query_one_as[T](...) -> Result[T?]` | Rows decoded into a `@derive(Json)` type by column name: numbers, booleans and `json`/`jsonb` columns to fields of those types, `timestamptz` and `timestamp` to `datetime.DateTime` (a `timestamp` has no zone and is read as UTC), `date` to `datetime.Date`, NULL to `None`, anything else to `str`. Dates assume the server's default `DateStyle` (ISO) |
| `postgres.Row` | `get(column) -> str?` (the text PostgreSQL sends), `text`, `int`, `float`, `bool` (each `Result`, an error for NULL), `columns()` |
| `postgres.DbError` | `code` (the SQLSTATE, such as `23505` for a unique violation; `08006` when the connection failed) and `reason`. Returned from a handler, a 500 |

Parameters are `$1`, `$2` in the SQL and a `list[str?]` beside it. They are sent separately from the statement, in text format, and the server converts each to the type the statement needs, so they cannot inject SQL. The language has no way to put an `int` and a `str` in one list without a wrapper, and text parameters are what `psql` uses, so `str(n)` is the price (D80).

The driver is runtime code over the same non-blocking socket as the HTTP server, so a query parks its task (D80). Authentication: SCRAM-SHA-256, MD5, cleartext and trust. `sslmode` is `disable`, `prefer` (the default: TLS when the server offers it, without checking the certificate, as libpq does), `require`, or `verify-full` (the certificate is checked against the trusted roots and the host name). A pool opens at most `pool_max` connections (10 by default); a task that finds them all busy waits up to 30 seconds. Not supported yet: `COPY`, `LISTEN`/`NOTIFY`, binary results, cancelling a running query, and statement caching (every query is parsed by the server each time).

## JSON

Two ways to work with JSON, both first-class.

### Typed: derive and decode

```python
@derive(Json)
struct Address:
    city: str
    zip: str

@derive(Json)
struct Customer:
    id: int
    name: str
    @json(name="emailAddress")
    email: str?
    addresses: list[Address] = []

customer = json.decode[Customer](body)?
text = json.encode(customer)
```

- `@derive(Json)` implements `ToJson` and `FromJson`. Works on structs and enums. Supported field types: numerics, `bool`, `str`, `T?`, `list[T]`, `dict[str, V]`, tuples, and other `Json` types.
- A type written as a JSON string implements the prelude trait `JsonString` instead: `to_json_string(self) -> str` and a static `from_json_string(text) -> Result[Self, str]`, whose error message becomes the decode error, with its JSON path. Implementing it implements `ToJson` and `FromJson`, so the type works as a field, in `json.encode` and `json.decode`, and in route bodies. OpenAPI documents it as a string. `datetime.DateTime`, `Date` and `Duration` are written this way (D110).
- Decoding rules: unknown keys are ignored by default (`@json(strict)` on the struct rejects them); a missing key is an error unless the field has a default or is optional; type mismatches report the JSON path (`addresses[1].zip: expected string, found number`).
- Enums encode as a tagged object by default (`{"type": "Text", "body": "..."}`); fieldless enums encode as strings.
- As built (M7): output is compact (`{"a":1}`); a struct field's default is used when its key is missing; tuples are arrays; `json.Value` derives `Eq`. See D47 and D48 in [08](08-decisions-and-risks.md).

### Dynamic: on the fly

```python
data = json.parse(body)?                     # json.Value
name = data["user"]["name"].as_str()         # str?  (None if absent or not a string)
for item in data["items"].as_list().unwrap_or([]):
    print(item["sku"].as_str().unwrap_or("?"))

payload = json.object({"ok": True, "count": 3, "tags": ["a", "b"]})
text = json.encode(payload)
```

- `json.Value` is an enum: `Null`, `Bool`, `Int`, `Float`, `Str`, `Array(list[Value])`, `Object(dict[str, Value])`.
- Indexing a `Value` never panics. A missing key or wrong type yields `Null`, so chained access is safe and the type is checked once at the end with `as_str`, `as_int`, `as_float`, `as_bool`, `as_list`, `as_dict`, each returning an option.
- `Value` implements `Json`, so a typed struct can contain a `json.Value` field for the parts whose shape is not known.

### Implementation and why it is fast

- The **scanner** (whitespace, structural characters, string unescaping, number parsing) and the **writer** (escaping, number formatting) are native runtime code working on byte buffers.
- The **structure** is compiled, not interpreted. For each derived type the compiler generates an encoder that appends field names as pre-escaped constants and calls the writer for values, and a decoder that reads keys and dispatches with a switch on length and bytes straight into the struct's fields. No intermediate `Value` tree is built and no reflection or field-name lookup happens at run time. This is the same strategy as Rust's `serde` and is the main reason typed JSON can compete with Go's generated-code encoders and beat its reflection-based default.
- Strings without escapes are copied once from the input buffer.
- `http.json(value)` encodes directly into the response buffer.

## Dates and times (docs/14, milestone 5)

```python
import datetime

now = datetime.now()                                    # a DateTime, in UTC
t = datetime.parse("2026-11-20T09:30:00+01:00")?        # RFC 3339; keeps its +01:00
t.utc().format_iso()                                    # "2026-11-20T08:30:00Z"
london = datetime.zone("Europe/London")?
t.to_zone(london).format("%Y-%m-%d %H:%M %Z")           # "2026-11-20 08:30 GMT"
later = t.add(datetime.hours(2))                        # DateTime + Duration
later.since(t)                                          # Duration: 2h0m0s
datetime.date(2026, 11, 20).weekday()                   # 4: Friday, counting Monday as 0
datetime.at(2026, 3, 8, 2, 30, zone=datetime.zone("America/New_York")?)   # 03:30 EDT: 02:30 never happened
datetime.parse_duration("1h30m")?.seconds()             # 5400.0
```

| Item | Shape |
|---|---|
| `datetime.DateTime` | An instant to the nanosecond, with the zone it is shown in. `==`, `<`, `hash`, `sorted` and `max` look at the instant alone, so the same moment in two zones is equal. `unix()`, `unix_millis()`, `nanosecond()`; `year()`, `month()`, `day()`, `hour()`, `minute()`, `second()`, `weekday()` and `date()` in its zone; `zone()`, `utc_offset()`; `to_zone(zone)`, `in_zone(name) -> Result`, `utc()`; `add(d)`, `sub(d)`, `since(other) -> Duration`; `format_iso()`, `format(pattern)`, `http_date()`. It prints in RFC 3339 |
| `datetime.Date` | A calendar day: `year()`, `month()`, `day()`, `weekday()`, `day_of_year()`, `add_days(n)`, `days_since(other)`, `at(hour=0, minute=0, second=0, zone=UTC) -> DateTime`, `format(pattern)`. `==`, `<` and `hash`. It prints as `2026-11-20` |
| `datetime.Duration` | Signed nanoseconds, up to about 292 years either way. `+`, `-`, `==`, `<`; `nanoseconds()`, `microseconds()`, `milliseconds()` (whole, rounded toward zero), `seconds()`, `minutes()`, `hours()` (floats), `times(n)`, `abs()`, `is_negative()`. It prints in Go's syntax: `1h30m0s`, `1.5s`, `300ms`, `0s` |
| `datetime.Zone` | `name()`, `offset_at(t) -> Duration`, `is_dst(t)`. Made by `datetime.utc()`, `datetime.local()` (`TZ`, else `/etc/localtime`, else UTC), `datetime.zone(name) -> Result` (IANA names such as `"Europe/London"`; `"UTC"` always works), `datetime.fixed_zone(offset)` |
| Making instants | `datetime.now()` (UTC), `datetime.at(year, month, day, hour=0, minute=0, second=0, nanosecond=0, zone=UTC)`, `datetime.from_unix(seconds, nanosecond=0)`, `datetime.from_unix_millis(ms)`, `datetime.parse(text) -> Result` |
| Making dates | `datetime.date(year, month, day)`, `datetime.today(zone=UTC)`, `datetime.parse_date(text) -> Result` (`YYYY-MM-DD`) |
| Making durations | `nanos(n)`, `micros(n)`, `millis(n)`, `seconds(n)`, `minutes(n)`, `hours(n)`, `days(n)` (24-hour days), all from `int`; `parse_duration(text) -> Result` |
| `datetime.sleep(d)` | `time.sleep` for a `Duration` |
| `datetime.set_clock_for_tests(t)` | Makes `now()` and `today()` return `t` in every task until it is called with `None` |
| `datetime.DateTimeError` | Text that is not a date, time or duration, or a zone that cannot be loaded. `reason`; returned from a handler, a 400 |

- **Parsing.** `parse` accepts RFC 3339 and the ISO 8601 forms near it: `T`, `t` or a space between date and time, seconds optional, up to 9 digits of fractional seconds after `.` or `,`, and `Z` or an offset written `+01:00`, `+0100` or `+01` (the form PostgreSQL sends). An offset is required. The error names the unexpected character and its position: ``unexpected 'X' at position 11 in "2026-11-20X09:30:00Z": expected `T` or a space between the date and the time``. Years run from 1 to 9999.
- **Formatting.** `format_iso()` is RFC 3339 with `Z` for a zero offset and only the fractional digits it needs. `format(pattern)` takes `%Y %m %d %H %M %S %f` (microseconds, 6 digits) `%z` (`+0100`) `%Z` (`BST`, or the offset for a fixed zone) `%a %A %b %B %j %%`; any other directive panics, since a pattern is part of the program. `http_date()` is the IMF-fixdate HTTP headers use.
- **Wall-clock times in a zone.** `at(..., zone=z)` and `Date.at` turn a local time into an instant. A time that happens twice, as clocks go back, is the earlier of the two; a time that never happens, as clocks go forward, moves forward by the size of the gap (02:30 on the morning New York springs forward is 03:30 EDT). Python and Go do the same.
- **Arithmetic is on instants.** `t.add(datetime.days(1))` is 24 hours later, which is not the same time of day across a daylight saving change; for calendar days use `t.date().add_days(1).at(...)`. The operators `+` and `-` are not defined between a `DateTime` and a `Duration` (D110); the compiler's error says to use `add` and `sub`.
- **Time zones** come from the TZif files (RFC 8536, versions 1 to 3) in `TZDIR`, or `/usr/share/zoneinfo` when it is not set, read once per name and cached. Past the end of a file's table (2037 in the standard build), the POSIX TZ rule in its footer gives the transitions. Windows has no zone database: set `TZDIR` to a copy of one, or use only UTC and fixed offsets.
- **JSON and OpenAPI.** All three types implement `JsonString`: a `DateTime` is an RFC 3339 string, a `Date` is `YYYY-MM-DD`, and a `Duration` is Go's syntax. OpenAPI documents them as `{"type": "string", "format": "date-time"}`, `"format": "date"`, and a string. A handler parameter of one of them is read from the path or the query: `@http.get("/days/{day}")` with `day: datetime.Date` answers `/days/2026-02-30` with a 400 saying the day is out of range.

## UUIDs (docs/14, milestone 6)

```python
import uuid

id = uuid.v7()                              # time-ordered: 019aa3f2-5c41-7d0e-9b2a-4e8f1c6d3a70
token = uuid.v4()                           # 122 random bits
same = uuid.parse(str(id))?                 # either case
same == id, id.version(), id.time()         # True, 7, the DateTime it was made, to the millisecond

@http.get("/orders/{id}")
def get(self, id: uuid.Uuid) -> Order:      # a malformed ID is a 400
    ...
```

| Item | Shape |
|---|---|
| `uuid.Uuid` | 16 bytes. `version()`, `bytes()`, `time() -> DateTime?` (for version 7). `==`, `<`, `hash` and `sorted` compare the bytes, so version 7 UUIDs sort by time. It prints, and is written to JSON, as the lowercase hyphenated form |
| `uuid.v4()` | A random UUID, from the operating system's secure random source |
| `uuid.v7()` | A time-ordered UUID: the Unix time in milliseconds, a counter, and random bits. Each is greater than every one made before it in the process |
| `uuid.parse(text) -> Result` | 8-4-4-4-12 hex digits with hyphens, in either case |
| `uuid.from_bytes(data) -> Result`, `uuid.nil()` | From 16 bytes; the all-zero UUID |
| `uuid.UuidError` | Text that is not a UUID, or bytes that are not 16. `reason`; returned from a handler, a 400 |

- **Which version.** Use `v7` for database keys and anything listed in order: UUIDs made together are near each other in an index, which random ones are not. Use `v4` where an ID must not reveal when it was made.
- **Order within a millisecond.** `v7` follows RFC 9562, section 6.2, method 1: within one millisecond a 42-bit counter, started at a random value below 2^41, counts up, so at least two trillion UUIDs fit before the timestamp is moved on by a millisecond. When the clock goes back, the timestamp stays at the last one used. Every UUID also has 32 fresh random bits.
- **Errors** say which character was wrong: ``expected a hex digit at position 36, found `z` ``. Braces, `urn:uuid:` and the 32-digit form without hyphens are not accepted.
- **Where they appear.** `http.request_log()` makes its request IDs with `v7`, so they sort by when requests arrived. OpenAPI documents a `Uuid` as `{"type": "string", "format": "uuid"}`. `datetime.set_clock_for_tests` fixes the time `v7` uses.
- **Random bytes** are read from the operating system 4 KiB at a time per thread, so a UUID costs a system call only once in about 400.

## Configuration (docs/14, milestone 7)

```python
import config
import datetime

@derive(Json, Display)
struct Settings:
    database_url: config.Secret                                  # DATABASE_URL, required
    port: int = 8080                                             # PORT=9000
    debug: bool = False                                          # DEBUG=yes
    allowed_origins: list[str] = []                              # ALLOWED_ORIGINS=https://a.example, https://b.example
    request_timeout: datetime.Duration = datetime.seconds(30)    # REQUEST_TIMEOUT=45s

def main():
    settings = config.load[Settings]().unwrap_or_else(lambda e: panic(e.message()))
    log.info(f"starting with {settings}")         # ... database_url=***, port=9000, ...
    db = postgres.connect(settings.database_url.reveal()).unwrap()
```

A deployment that gets two variables wrong learns about both at once:

```text
config: DATABASE_URL is required; PORT must be an integer, got "80a"
```

| Item | Shape |
|---|---|
| `config.load[T](prefix="", env_file=".env") -> Result[T, ConfigError]` | `T` is a struct deriving `Json`. Field `port` is read from `PREFIX + PORT`. The variables are the environment's, and those of `env_file` that the environment does not set, if the file exists (`""` reads none). The environment is not changed |
| `config.load_env(path=None, override=False) -> Result[None, ConfigError]` | Sets the variables of a `.env` file that are not set already, or all of them with `override`. Without `path`, `.env` is read if it exists; a `path` that does not exist is an error. A malformed file sets nothing |
| `config.Secret`, `config.secret(value)` | A string that prints as `***`: in `str()`, f-strings, a derived `Display` (and so in `log.info(f"{settings}")`) and JSON. `reveal()` returns it, `is_empty()` checks it. Read from a variable like a `str`; made in code with `config.secret` |
| `config.ConfigError` | `reason` (all the problems, joined) and `problems` (one string each). Returned from a handler, a 500 |

How a variable's text becomes a field:

| Field type | Text |
|---|---|
| `str` | As it is |
| `int`, `float` | Trimmed, then parsed: `8080`, `0.25`. Not `inf` or `nan` |
| `bool` | `true`, `1`, `yes`, `on` or `false`, `0`, `no`, `off`, in any case |
| A type with a string form (`datetime.Duration`, `datetime.Date`, `uuid.Uuid`, `config.Secret`, users' `JsonString` types) | Its own parser: `45s`, `2026-11-20` |
| `list[...]` | Comma-separated, each item trimmed and empty ones dropped (`a, b, c`), or JSON when it starts with `[` (`["a", "b,c"]`, `[1, 2]`) |
| A struct or `dict` | JSON: `{"burst": 10}` |
| `T?`, or any field with a default | May be left unset |

Inside JSON written in a variable, numbers and booleans may also be strings, as at the top level. A problem inside a nested value names the variable and the path: `LIMITS at burst must be an integer, got "x"`.

**The `.env` file**, exactly:

- One `KEY=value` per line, with an optional `export ` before the key. A key is a letter or `_`, then letters, digits and `_`. Spaces may surround the `=`.
- Blank lines and lines starting with `#` are ignored.
- An unquoted value runs to the end of the line, or to a `#` after a space or tab (so `http://x/#top` keeps its `#`), and is trimmed. `KEY=` is the empty string.
- A value in single quotes is taken as written. A value in double quotes reads `\n`, `\r`, `\t`, `\"` and `\\` as escapes and keeps any other backslash. Either may span lines, as a PEM key does. Only spaces and a `#` comment may follow the closing quote.
- There is no `${VAR}` interpolation (docs/14, decision 7). A UTF-8 byte order mark is ignored, `\r\n` ends a line as `\n` does, and the last line for a key wins.
- Anything else is an error naming its line: ``config: .env, line 4: expected `=` after `BROKEN`, found `v` ``. A line is never skipped.

- **Precedence.** The real environment wins over the file, so a deployment overrides what is checked in; `load_env(override=True)` reverses that for `load_env`.
- **Keep `.env` out of version control.** Commit a `.env.example` with every variable and no real secrets, as the examples in `examples/` do.
- **How it works.** `load` builds a JSON object of the variables (lower-cased, prefix removed) and decodes it with the derived `FromJson` code in a lenient mode (D112). The mode turns a string into the number, boolean, list or object expected, and, at the top level only, records a field that fails or is missing and goes on with the next, so one decode finds every problem.

## Metrics and health (docs/14, milestone 8)

```python
import http
import metrics

@frozen
struct Stats:
    jobs_done: metrics.Counter
    db_time: metrics.Histogram

def main():
    stats = Stats(
        metrics.counter("jobs_done_total", "Jobs finished", labels=["queue"]),
        metrics.histogram("db_query_seconds", "Query time"),
    )
    server = http.Server()
    server.drain_delay = 5                                  # seconds of failing /readyz after SIGTERM
    server.metrics()                                        # GET /metrics and /metrics/dashboard, METRICS_USER and METRICS_PASSWORD
    server.health(checks={"database": lambda: ping(db)})    # GET /healthz and /readyz
    ...

def send(stats: Stats, job: Job) -> Result[None]:
    with stats.db_time.time():
        save(job)?
    stats.jobs_done.inc({"queue": job.queue})
```

**`server.metrics(path="/metrics", dashboard=True, user=None, password=None, public=False)`** serves every metric in the Prometheus text format, version 0.0.4, with `Content-Type: text/plain; version=0.0.4; charset=utf-8` and `Cache-Control: no-store`. Recording requests starts when it is called; a server that never calls it reads no clock and touches no counter for its requests.

- **Credentials.** HTTP Basic authentication, with the user name and password from the arguments, else from the environment variables `METRICS_USER` and `METRICS_PASSWORD` (`config.load_env()` puts a `.env` file's there). A request without them gets a 401 with `WWW-Authenticate: Basic realm="metrics", charset="UTF-8"`. Without credentials, and without `public=True`, the call panics: ``server.metrics needs a username and password: set METRICS_USER and METRICS_PASSWORD in the environment or in .env (read by config.load or config.load_env), or pass public=True to serve the metrics without them.`` The runtime keeps only HMAC-SHA256 digests of the two, under a key made for the process, and compares digests in constant time.
- **A password hash.** `METRICS_PASSWORD_HASH` may take the place of `METRICS_PASSWORD`, so the password itself is in no file: `viper hash-password` asks for it twice without echo (or reads one line from standard input when that is not a terminal) and prints `pbkdf2-sha256$<iterations>$<salt>$<hash>`, PBKDF2-HMAC-SHA256 with a 16-byte random salt and 600,000 iterations (`--iterations` changes it), salt and hash in standard base64. Checking a password against it takes a few hundred milliseconds; once one has matched, its HMAC digest is kept, so the dashboard's polling is checked as cheaply as a plain password. A value that is not in that form stops the program at `server.metrics` with ``METRICS_PASSWORD_HASH is not a password hash: make one with `viper hash-password`, which prints pbkdf2-sha256$<iterations>$<salt>$<hash>``.
- **Lock-out.** After 10 wrong user names or passwords from one `req.client_ip()` within a minute, that address gets `429 Too Many Requests` with `Retry-After` for a minute, even with the right password, and a `warn` line is logged. Requests without an `Authorization` header (a browser's first try) are not counted. The table holds 4096 addresses.
- **Plain HTTP.** The first time credentials arrive over plain HTTP from an address that is not loopback (not TLS, and not through a trusted proxy that sent `X-Forwarded-Proto: https`), a `warn` line says that they cross the network readably.
- **Headers.** Every response from these routes has `X-Frame-Options: DENY`, `X-Content-Type-Options: nosniff` and `Referrer-Policy: no-referrer`; all but the dashboard's stylesheet and script have `Cache-Control: no-store`.
- The routes are left out of the metrics and of the API documentation. Middleware applies to them, and `request_log` logs them.

**The dashboard**, with `dashboard=True` (the default), is a page at `path + "/dashboard"` that charts the metrics over the last hour, for a quick look without Prometheus or Grafana. Its HTML, stylesheet and script are embedded in the runtime (no CDN, fonts or chart library; under 60 KB together), served with ETags, and left out of programs that never call `server.metrics`. The page has a strict `Content-Security-Policy` (`default-src 'self'`, no inline script or style), refreshes every 5 s, has a pause button and a 5 minute, 15 minute or 1 hour range, follows the browser's light or dark mode, and works at phone width. It shows:

- tiles: uptime and start time, requests per second over the last minute and in total, the share of 5xx responses, p50, p95 and p99 latency, requests in flight and open connections;
- charts: requests per second stacked by status class, latency p50, p90 and p99, requests in flight, connections and tasks, and the collected heap and resident memory;
- each route, sortable and filterable: requests, requests per second over the last minute, 4xx and 5xx shares, p50, p95 and p99 since the start;
- the last 50 responses with a status of 500 or more, and the 20 slowest requests of the last five minutes, each with its time, method, path **without its query string** (queries often carry tokens), status, duration and request ID, to find in the access log;
- the program's own counters and gauges, each with a sparkline, and its histograms' counts.

Percentiles are estimated from the duration histogram's buckets by linear interpolation, as Prometheus' `histogram_quantile` does, and the page says so. The history is a ring of 720 samples taken every 5 s by a thread that `server.metrics` starts (about 0.5 MB when 20 routes are busy; a sample keeps only the routes that had requests); it is kept in memory and starts again with the process. `VIPER_METRICS_INTERVAL_MS` changes the interval, down to 50 ms, for tests. The page reads it from `path + "/dashboard/data.json?since=<sample>"`, which returns only the samples after `since`, the totals, the routes and the recent events. Recording an event costs one comparison per request, and a short lock only for a request that is a server error or among the slowest. `examples/metrics_dashboard` is a small service with the dashboard, a `.env.example` and tests.

What is recorded:

| Metric | Type | Labels | Notes |
|---|---|---|---|
| `http_requests_total` | counter | `method`, `route`, `status` | `route` is the route's pattern (`/todos/{id}`), never the path; `unmatched` for requests no route took (404s, 405s). `method` is one of the seven standard ones or `_OTHER`; `status` the exact code |
| `http_request_duration_seconds` | histogram | `method`, `route` | From the request being read to its response being ready. Buckets 0.1, 0.25, 0.5, 1, 2.5, 5, 10, 25, 50, 100, 250, 500 ms, 1, 2.5, 5, 10 s. `method` is the route's own (a `HEAD` served by a `GET` route counts under `GET`); `unmatched` has only `route` |
| `http_response_size_bytes` | summary | `method`, `route` | `_sum` and `_count` of body bytes sent (none for `HEAD` and 304) |
| `http_requests_in_flight`, `http_connections_open` | gauge | | |
| `http_connections_total` | counter | | Accepted since recording began |
| `http_request_errors_total` | counter | `kind` | `panic` (a handler panicked: a 500), `timeout` (a client too slow to send its request: 408), `parse` (400, 505), `too_large` (a body past its limit: 413, which is also counted as a request; a head too large: 414, 431) |
| `process_start_time_seconds`, `process_resident_memory_bytes`, `process_cpu_seconds_total` | gauge, gauge, counter | | From `/proc` |
| `runtime_heap_bytes`, `runtime_gc_collections_total` | gauge, counter | | From the collector |
| `runtime_tasks`, `runtime_worker_threads` | gauge | | |
| `build_info` | gauge | `version` | Always 1 |

- **A WebSocket** counts as a 101 in `http_requests_total` and is not timed.
- **More than 1024 routes:** the rest share `route="other"`, with a warning when the 1025th is added.

**Application metrics**, the `metrics` module:

| Item | Shape |
|---|---|
| `metrics.counter(name, help, labels=[]) -> Counter` | `inc(labels={}, by=1.0)`; `by` may not be negative. Name it ending in `_total` |
| `metrics.gauge(name, help, labels=[]) -> Gauge` | `set(value, labels={})`, `inc(labels={}, by=1.0)`, `dec(labels={}, by=1.0)` |
| `metrics.histogram(name, help, labels=[], buckets=[]) -> Histogram` | `observe(value, labels={})`; `time(labels={})` for a `with` block, observing its seconds however it is left. Without `buckets`, the request histogram's, in seconds |
| `metrics.text() -> str` | What `/metrics` serves |
| `metrics.snapshot_for_tests() -> dict[str, float]` | Every series by its name and labels as the text writes them: `jobs_done_total{queue="email"}`, `db_query_seconds_count` |

- A metric is a `@frozen` handle into the runtime's registry: make it once, in `main`, and pass it to handlers and tasks, in a `@frozen` struct of the program's metrics or in the server's state. Calling `metrics.counter` again with the same arguments also gives the same metric. An update takes a read lock and changes atomics; only a new set of label values takes a write lock and allocates.
- Registering the same name again with the same kind, labels and buckets gives the same metric. Another kind or other labels, a name that is not `[a-zA-Z_:][a-zA-Z0-9_:]*`, a label `le` on a histogram, and the runtime's own names panic, as does calling with the wrong set of labels: `the metric jobs_done_total takes the labels queue, but was given queue, x`.
- **Label values come from a small set** (a queue's name, not a user's ID). After 1000 sets of values in one metric, new ones are counted under `"other"` for every label, with one warning logged.
- Label values are escaped as the format requires (a backslash, a double quote and a newline as `\\`, `\"` and `\n`), and help text likewise apart from the quote.

**`server.health(live="/healthz", ready="/readyz", checks={})`:**

- `live` answers `200 ok` while the process serves.
- `ready` runs the checks, `dict[str, def() -> Result[None]]`, at once, each in a task of its own, and waits up to two seconds in all. It answers 200 with `{"cache":"ok","database":"ok"}` when every check passes, otherwise 503 with each failure's message in place of `"ok"` (`"did not answer within 2 s"` for one too slow). The checks are run in other tasks, so the compiler checks that they are shareable; write the dict in the call.
- **On SIGTERM** (or `server.stop()`), `ready` answers 503 `{"server":"draining"}` at once, and the server goes on serving everything for `server.drain_delay` seconds before it stops accepting, so that a load balancer has stopped sending requests before connections close. SIGINT stops at once.
- Neither route is in the access log, the metrics or the API documentation. Both send `Cache-Control: no-store`.

## Traces (docs/14, milestone 10)

```python
import http
import trace

def main():
    trace.init("photos")      # or from OTEL_EXPORTER_OTLP_ENDPOINT alone, with http.request_log(trace=True)
    server = http.Server()
    server.use(http.request_log())
    server.get("/photos/{id}", photo)
    ...

def resize(photo: Photo, width: int) -> Photo:
    with trace.span("resize image", {"width": str(width)}) as span:
        span.set("photo.id", str(photo.id))
        ...
```

Spans go to an OpenTelemetry collector (Jaeger, Tempo, Honeycomb and others take OTLP) as OTLP/JSON over HTTP, `POST <endpoint>/v1/traces`. Once tracing is on:

- **Every request a server answers is a span** named `GET /photos/{id}` (the route's pattern, or the method alone when no route took it), of kind server, with `http.request.method`, `http.route`, `url.path`, `http.response.status_code` and `client.address`; a 5xx or a panic marks it failed. A request with a valid W3C `traceparent` header continues that trace, as a child of the caller's span, and its `tracestate` is kept. The metrics' and health checks' routes are left out.
- **Every `http.fetch` is a client span** of the current one, named by its method, with `url.full` (without the query string), `server.address` and `server.port`, failed on a status of 400 or more or an error. It sends `traceparent` (and `tracestate`) on, unless the call set one itself.
- **Every PostgreSQL query is a client span** named by the statement's first word (`SELECT`, `INSERT`), with `db.system` and `db.operation.name`; never the statement's values.
- **`trace.span(name, attributes={})`** in a `with` statement is a span of the program's own around the block, ended however the block is left. Spans nest through the task: a span started in a handler is a child of the request's, and a task spawned in one starts with its parent's span.
- **Log lines** written during a span carry `trace_id` and `span_id` after the `request_id`, in text and JSON alike, so a collector's trace leads to its lines and back.

| Item | Shape |
|---|---|
| `trace.init(service="", endpoint=None, ratio=None) -> bool` | Turns tracing on and returns `True`. The endpoint is the collector's base URL (`http://localhost:4318`), else `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` (the full URL) or `OTEL_EXPORTER_OTLP_ENDPOINT`; without one, tracing stays off and it returns `False`. The service is `OTEL_SERVICE_NAME`, else `service`, else the program's file name. `ratio` (0 to 1, else `OTEL_TRACES_SAMPLER_ARG`, else 1) is the share of new traces recorded; a trace that arrives with a request keeps its caller's choice. Once per process. Panics on a ratio outside 0 to 1 or a URL that is not `http://` or `https://` |
| `trace.span(name, attributes={}) -> trace.Span` | For `with`; `set(key, value)` and `fail(message)` |
| `trace.trace_id() -> str` | The current trace's 32 hex digits, `""` outside one |
| `trace.traceparent() -> str` | The current span as a `traceparent` value, for passing a trace on by other means (a queue message) |
| `trace.flush()` | Sends what is waiting now, waiting up to 5 s. The program does this itself when it ends, as `os.exit` does |
| `http.request_log(trace=True)` | Turns tracing on from the environment, as `trace.init()` would, so a service needs no code to be traced |

- **Sampling.** A trace that is not recorded still has IDs, and still passes them on with its `sampled` flag clear, so that the services after this one make the same choice.
- **Exporting.** Ended spans wait in a queue of at most 2048 and are sent by a thread of their own in batches of up to 512, every 5 s or sooner when 512 are waiting, with a 10 s timeout. When the queue is full, or the collector cannot be reached or refuses a batch, spans are dropped, not retried: one `warn` line says so (`could not send 12 spans to http://...: connection refused; dropping them until it works`), and an `info` line says when sending works again. Requests never wait for the collector.
- **On `/metrics`**, while tracing is on: `trace_spans_exported_total` and `trace_spans_dropped_total`.
- **Cost.** With tracing off, a request, a fetch and a query each pay one load of a flag.
- For tests, `TRACE_INTERVAL_MS` with the language's prefix shortens the 5 s interval (to at least 10 ms).

## Tasks and synchronisation

API-level summary; semantics are in [04](04-runtime-and-memory.md#concurrency).

```python
def crawl(urls: list[str]) -> list[Page]:
    results = Channel[Page](len(urls))
    for url in urls:
        spawn fetch_into(url, results)
    return [results.recv().unwrap() for _ in range(len(urls))]

def with_timeout():
    task = spawn slow_query()
    match task.join_timeout(time.seconds(2)):
        case Some(rows):
            use(rows)
        case None:
            log.warn("query timed out")
```

| Item | Notes |
|---|---|
| `spawn f(args)` | Keyword; the call's arguments are evaluated in the spawning task |
| `Task[T]` | `join`, `join_timeout`, `is_done` |
| `Channel[T](capacity)` | `send`, `recv`, `try_recv`, `close`, iteration |
| `Mutex[T](value)` | `lock()` returns a guard usable with `with` |
| `Atomic` | `get`, `set`, `add`, `increment`, `decrement`, `compare_and_set`; holds an `int` |
| `time.sleep(seconds)` | Pauses the calling task only; `time.monotonic()` and `time.now()` give clocks in seconds. `datetime.sleep(d)` takes a `Duration` |

What may be passed to `spawn`, sent on a channel or put in a `Mutex` is checked at compile time ([04](04-runtime-and-memory.md#data-race-prevention)); in the example above `Page` must be a `@frozen` struct.

Cancellation is not in the prototype. The intended design is a deadline carried by the request and checked by waiting operations, similar in purpose to Go's `context` but implicit.

## What lives where

| Component | Compiler built-in | Rust runtime | Viper source |
|---|---|---|---|
| `int`, `float`, `bool` arithmetic | ✔ | | |
| `list` indexing and length | ✔ (inline) | growth, sort | methods (later: everything) |
| `dict`, `set` | type only (a set is a dict without values) | implementation | `set` methods in the prelude |
| `str` | literals, length | operations | convenience methods |
| f-strings | ✔ (desugaring) | number formatting | |
| `Option`, `Result` | coercions, `?` | | defined as enums, methods |
| Derivations | ✔ | | |
| Tasks, channels, mutexes | `spawn` keyword | implementation | typed wrappers |
| JSON | derive | scanner, writer | `Value`, API |
| HTTP, WebSocket | | protocol, router, I/O | `Server`, `Request`, `Response`, helpers |
| TLS | | `rustls` | `bind_tls`, `listen_tls` |
| `crypto` | | `ring` digests, HMAC, RSA; base64 | API |
| `auth` | `on_login` and `protect` checked shareable | | everything |
| PostgreSQL | | protocol, pool | `Db`, `Row`, `query_as` |
| `log` | | formatting, the request context, buffering | API, `Logger` |
| `time`, `io`, `os` | | system calls | API |

The direction of travel is rightwards: as generics, byte buffers and the optimiser mature, implementations move from the runtime column to the Viper column, which makes them inlinable and lets the language test itself.
