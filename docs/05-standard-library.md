# 05 — Standard library

The standard library is what makes Viper a backend language and not a general-purpose one. Its API is written in Viper (`stdlib/*.vi`). What sits behind the API moves over time from the Rust runtime into Viper itself.

## Layers

| Module | Purpose | Milestone | Implemented in (prototype) |
|---|---|---|---|
| prelude | `print` (with `sep`, `end` and `flush`), `len`, `range`, `str`, `int`, `float`, `list`, `ord`, `chr`, `reversed`, `min`, `max`, `sorted`, `panic`, built-in types and traits | M1–M4 | Compiler built-ins plus runtime |
| `io`, `os` | Files, standard streams, environment variables, arguments | M5 | Viper wrappers over runtime |
| `time` | Clocks, durations, `sleep` | M6 | Viper wrappers over runtime |
| `datetime` | Instants, dates, durations, time zones, RFC 3339 | docs/14, milestone 5 | Viper, over runtime parsing, formatting and TZif reading |
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
| `os` | `args() -> list[str]`, `getenv(name) -> str?`, `setenv(name, value)` (at startup, before starting tasks: the environment is not safe to change while other threads read it), `exit(code)`, `cwd()` |

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
2026-11-20T09:30:01.456Z WARN  slow query request_id=6f1c0e2a9b3d4c58 ms=812.4 table=orders
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
| 4 | Name equals a `{name}` in the path | That path segment, parsed as the parameter's type: `int`, `float`, `bool` or `str` | 400 |
| 5 | Type derives `Json`, or is a `list` or `dict` of such | The JSON request body. At most one per handler | 400, with the JSON path of the problem |
| 6 | `int`, `float`, `bool` or `str` whose name is not in the path | The query parameter of that name. Required unless the type is `T?` or the parameter has a default | 400 |
| 7 | Type `http.Header` or `http.Header?` (M12) | The request header named after the parameter: `user_agent` is `User-Agent` | 400 if missing, unless `T?` |
| 8 | Type `http.Cookie` or `http.Cookie?` (M12) | The cookie with the parameter's name | 400 if missing, unless `T?` |
| 9 | Type `auth.User` or `auth.User?` (D104), matched before rule 5 although `auth.User` derives `Json` | The signed-in user. At most one per handler, and not under `@http.ws` | `auth.User` protects the route: signed out is a 401, or a redirect to sign in for a browser. `auth.User?` is `None` |

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
| `server.get/post/put/patch/delete/head/options(path, handler)` | Register one route in call style |
| `server.route(method, path, handler)` | General form |
| `server.group(prefix)` | Returns a sub-router sharing a prefix and its own middleware |
| `server.use(middleware)` | Middleware has the type `def(http.Request, http.Handler) -> Result[http.Response]` |
| `http.request_log(skip=[])` | Middleware that gives each request an ID and logs one line when it is answered: `GET /photos/7 200` with the fields `method`, `path` (without the query string), `status`, `ms` and, for a request from a connection, `client` (`req.client_ip()`); `error` for a 5xx, otherwise `info`. The ID is the client's `X-Request-Id` when it is 1 to 128 ASCII letters, digits, `.`, `_` and `-`, otherwise a new one; it is sent back in `X-Request-Id` (error responses included), kept as the request local `"request_id"`, and carried by log lines (see "Logging"). Paths in `skip` get neither. Add it first, so it times the other middleware too. A handler that panics gets no line: the panic is reported instead |
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
| `http.Handler` | `def(http.Request) -> Result[http.Response]` |
| `http.Request` | `method`, `path`, `param(name) -> str`, `param_int(name) -> Result[int]` (fails with a 400 `http.Error`), `query(name) -> str?`, `query_string() -> str` (as sent, without `?`), `header(name) -> str?`, `cookie(name) -> str?` (`%XX` decoded), `body() -> bytes` (any bytes, for binary uploads), `text() -> Result[str]`, `json[T]() -> Result[T]`, `remote_addr() -> str` (the connection's other end, `host:port`), `client_ip() -> str` (the client's address without a port: through `server.trusted_proxies`, the rightmost address in `X-Forwarded-For` that is not one of them, so a client cannot choose it; otherwise the connection's; `""` for a test request); `set_local(name, value)` and `local(name) -> str?` keep values with the request for the handlers after a middleware (docs/13) |
| `http.Response` | `status`, `headers`, `body`; built with `http.text`, `http.json`, `http.binary(data, content_type="application/octet-stream", status=200)` (a body of any bytes), `http.error`, `http.redirect`, `http.no_content`; `body_bytes()` is the body as sent, for tests. `with_header(name, value)` sets a header; `add_header(name, value)` adds another value, sent as its own line (in `headers`, the values are joined by newlines) |
| `response.set_cookie(name, value, max_age=-1, path="/", http_only=True, secure=False, same_site="Lax")` | Adds a `Set-Cookie` header; any number per response. Characters a cookie cannot hold, and `%`, are sent as `%XX`, which `req.cookie` decodes. `__Host-` and `__Secure-` cookies, and `same_site="None"`, are always `Secure`. An invalid name, path or `same_site` is a bug and panics (a 500). `delete_cookie(name, path="/")` expires one; `response.cookie(name) -> str?` reads one back, for tests |
| `http.url_encode(text)`, `http.url_decode(text, plus_is_space=True) -> str?`, `http.form_encode([(name, value), ...])` | Percent-encoding: everything but ASCII letters, digits and `-._~` as `%XX` of its UTF-8 bytes. `form_encode` makes a query string or an `application/x-www-form-urlencoded` body |
| `http.test_request(method, path, body="", headers={})` | A request without a connection, for `server.handle(...)` in tests. A query string on `path` is what `query` reads; `headers` are what `header` and `cookie` read |
| `http.Error` | An `Error` carrying a status code |

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
- **JSON and OpenAPI.** All three types implement `JsonString`: a `DateTime` is an RFC 3339 string, a `Date` is `YYYY-MM-DD`, and a `Duration` is Go's syntax. OpenAPI documents them as `{"type": "string", "format": "date-time"}`, `"format": "date"`, and a string.

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
