# 05 — Standard library

The standard library is what makes Weby a backend language and not a general-purpose one. Its API is written in Weby (`stdlib/*.wb`). What sits behind the API moves over time from the Rust runtime into Weby itself.

## Layers

| Module | Purpose | Milestone | Implemented in (prototype) |
|---|---|---|---|
| prelude | `print`, `len`, `range`, `str`, `int`, `float`, `min`, `max`, `sorted`, `panic`, built-in types and traits | M1–M4 | Compiler built-ins plus runtime |
| `io`, `os` | Files, standard streams, environment variables, arguments | M5 | Weby wrappers over runtime |
| `time` | Clocks, durations, `sleep` | M6 | Weby wrappers over runtime |
| `sync` | `Channel`, `Mutex`, `Atomic`, `spawn` support | M6 | Runtime |
| `json` | Dynamic values, typed encode and decode | M7 | Runtime scanner and writer; compiler-derived per-type code |
| `net` | TCP listener and stream | M8 | Runtime |
| `http` | Server, router, request, response, middleware, WebSockets | M8–M9 | Runtime protocol code; Weby API layer |
| `log` | Levelled, structured logging to standard error | M8 | Weby |
| `testing` | Assertions and the `@test` runner | post-prototype | Weby |

Not in the prototype: HTTP client, TLS, database drivers, templating, regular expressions, cryptography beyond what the WebSocket handshake needs. An HTTP client and a PostgreSQL driver are the first two additions afterwards, since a backend without them is a toy.

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
| `os` | `args() -> list[str]`, `getenv(name) -> str?`, `exit(code)`, `cwd()` |

`input(prompt)` is also built in, returning `str?` (`None` at the end of input).

## Core types

Method lists are indicative; the prototype implements what the demo programs need, then fills in.

| Type | Operations |
|---|---|
| `str` | `len`, `+`, `==`, `in`, slicing, iteration; `upper`, `lower`, `strip`, `split`, `join`, `replace`, `find`, `starts_with`, `ends_with`, `to_bytes`; `int.parse(s)`, `float.parse(s)` return `Result` |
| `list[T]` | Literal `[a, b]`, indexing, slicing, `len`, `in`, iteration; `append`, `extend`, `insert`, `pop`, `remove`, `clear`, `sort`, `reverse`, `index`, `copy` |
| `dict[K, V]` | Literal `{k: v}`, `d[k]` (panics if missing), `d[k] = v`, `in`, `len`; `get(k) -> V?`, `get_or(k, default)`, `pop`, `keys`, `values`, `items` |
| `set[T]` | Literal `{a, b}`, `add`, `remove`, `in`, `len`, union and intersection |
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
| `http.get(url)`, `http.post(url, body, content_type="application/json")`, `http.fetch(method, url, body="", headers={})` | Send a request and wait for the whole response. A task waiting here parks; it does not hold a worker |
| `http.ClientResponse` | `status`, `body: bytes`, `ok()`, `header(name) -> str?`, `text() -> Result[str]`, `json() -> Result[json.Value]` |
| `http.decode_json[T](response)` | The body decoded into `T` |
| `http.ClientError` | No response: unreachable host, timeout (30 seconds), connection closed early, or a URL that is not `http://`. Returned from a handler with `?`, it becomes a 502 |

One request per connection, HTTP/1.1, `http://` only: TLS is not in the runtime yet (D77).

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

### API surface

| Item | Shape |
|---|---|
| `http.Server()` | Creates a server with an empty router |
| `server.mount(value_or_function, prefix="")` | Registers annotated routes (above) |
| `server.get/post/put/patch/delete/head/options(path, handler)` | Register one route in call style |
| `server.route(method, path, handler)` | General form |
| `server.group(prefix)` | Returns a sub-router sharing a prefix and its own middleware |
| `server.use(middleware)` | Middleware has the type `def(http.Request, http.Handler) -> Result[http.Response]` |
| `server.ws(path, handler)` | WebSocket endpoint in call style (below) |
| `server.listen(addr)` | Blocks, serving until shutdown. Options: read and write timeouts, maximum header and body sizes, graceful shutdown on SIGTERM |
| `http.Handler` | `def(http.Request) -> Result[http.Response]` |
| `http.Request` | `method`, `path`, `param(name) -> str`, `param_int(name) -> Result[int]` (fails with a 400 `http.Error`), `query(name) -> str?`, `header(name) -> str?`, `body() -> bytes`, `text() -> Result[str]`, `json[T]() -> Result[T]`, `remote_addr` |
| `http.Response` | `status`, `headers`, `body`; built with `http.text`, `http.json`, `http.bytes`, `http.error`, `http.redirect`, `http.no_content` |
| `http.Error` | An `Error` carrying a status code |

### Error mapping

When a handler returns `Err`, in either style:

- An `http.Error` becomes a response with its status and a JSON body `{"error": message}`.
- A JSON decode error from `req.json` becomes 400.
- Any other error becomes 500, is logged with its message, and the client sees a generic body.

The mapping is overridable with `server.on_error(handler)`.

### Implementation

As built in M8 (D49, D50), and since M10 with each connection on a fiber rather than a thread (D61, D65): the request parser, connection handling, limits and timeouts are in the runtime, written by hand rather than with `httparse`; the router, `Request`, `Response`, middleware and error mapping are written in Weby in `stdlib/http.lang`, with routes tried in order rather than through a radix tree. `http.Error(status, message)` is a function returning the struct `http.HttpError` (D51). `server.bind(addr)` and `server.serve()` split `listen` in two, so a program can learn its port (`":0"` picks a free one); `server.handle(req)` with `http.test_request(...)` answers a request without a socket, for tests.

The original plan for the prototype, inside the runtime:

- One accept loop. Each connection is handled by a task (stage 1: thread).
- Request parsing with the `httparse` crate into a reusable per-connection buffer. Headers are exposed to Weby lazily: a header is turned into a Weby string only when asked for.
- Routing with a radix tree (the `matchit` crate or equivalent). Route handlers are stored as handles ([04](04-runtime-and-memory.md#the-rust-heap-rule)).
- HTTP/1.1 keep-alive, `Content-Length` and chunked request bodies, `Expect: 100-continue`, `HEAD` handling, automatic `Date` and `Content-Length` response headers.
- Limits on header size, body size and idle time, on by default.
- Responses are serialised into the connection's write buffer and flushed with one system call where possible.

After the prototype: the router and the request/response types move into Weby; the byte-level parser may stay native indefinitely. TLS is added in the runtime with `rustls`. HTTP/2 is considered only if there is demand, since backends usually sit behind a proxy that speaks HTTP/1.1 upstream.

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
| `time.sleep(seconds)` | Pauses the calling task only; `time.monotonic()` and `time.now()` give clocks in seconds |

What may be passed to `spawn`, sent on a channel or put in a `Mutex` is checked at compile time ([04](04-runtime-and-memory.md#data-race-prevention)); in the example above `Page` must be a `@frozen` struct.

Cancellation is not in the prototype. The intended design is a deadline carried by the request and checked by waiting operations, similar in purpose to Go's `context` but implicit.

## What lives where

| Component | Compiler built-in | Rust runtime | Weby source |
|---|---|---|---|
| `int`, `float`, `bool` arithmetic | ✔ | | |
| `list` indexing and length | ✔ (inline) | growth, sort | methods (later: everything) |
| `dict`, `set` | type only | implementation | (later: implementation) |
| `str` | literals, length | operations | convenience methods |
| f-strings | ✔ (desugaring) | number formatting | |
| `Option`, `Result` | coercions, `?` | | defined as enums, methods |
| Derivations | ✔ | | |
| Tasks, channels, mutexes | `spawn` keyword | implementation | typed wrappers |
| JSON | derive | scanner, writer | `Value`, API |
| HTTP, WebSocket | | protocol, router, I/O | `Server`, `Request`, `Response`, helpers |
| `log`, `time`, `io`, `os` | | system calls | API |

The direction of travel is rightwards: as generics, byte buffers and the optimiser mature, implementations move from the runtime column to the Weby column, which makes them inlinable and lets the language test itself.
