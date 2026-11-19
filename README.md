# Weby

A compiled, Python-shaped language for web backends: indentation syntax, structs and traits instead of classes, an LLVM backend, and HTTP, TLS, WebSocket, JSON, PostgreSQL and concurrency support in the standard library.

**New to the language?** Start with **[Weby by example](docs/examples.md)**. It has twenty-five short sections, from "hello" to a JSON web service with tests. Every program in it is compiled and run by the test suite.

## Status

The prototype is complete and milestones 0 to 11 are done. Milestone 12 (performance and ecosystem) is ongoing by design. So far it has added:

- TLS, an HTTP client, a PostgreSQL driver and regular expressions;
- the first phase of the [performance plan](docs/09-performance-plan.md), after which sorting and list-heavy code run faster than Go.

The WebSocket server passes the Autobahn conformance suite, and the JSON parser passes JSONTestSuite. See the [roadmap](docs/06-roadmap.md#progress).

Source is lexed, parsed, type-checked and compiled to native code through LLVM 19, then linked with a Rust runtime. The runtime uses the Boehm garbage collector and runs tasks as fibers on one worker thread per core.

```python
trait Shape(Display):
    def area(self) -> float

    def describe(self) -> str:
        return f"{self} has area {self.area():.1f}"

@derive(Display)
struct Circle:
    radius: float

impl Shape for Circle:
    def area(self) -> float:
        return 3.14159 * self.radius ** 2

enum Command:
    Draw(size: float)
    Quit

def parse(text: str) -> Result[Command]:
    if text == "quit":
        return Quit
    return Draw(float.parse(text)?)

def main():
    for text in ["2", "quit", "big"]:
        match parse(text):
            case Ok(Draw(size)):
                shape: Shape = Circle(size)
                print(shape.describe())
            case Ok(Quit):
                print("bye")
            case Err(error):
                print("error:", error.message())
```

A web service: typed routes declared with annotations, a PostgreSQL pool shared by every request, rows decoded straight into structs, and HTTPS.

```python
import http
import os
import postgres

@derive(Json)
@frozen
struct Todo:
    id: int
    title: str
    done: bool

@derive(Json)
@frozen
struct NewTodo:
    title: str

@frozen
struct App:
    db: postgres.Db

impl App:
    @http.get("/todos/{id}")
    def get(self, id: int) -> Result[Todo]:
        sql = "select id, title, done from todos where id = $1"
        match postgres.query_one_as[Todo](self.db, sql, [str(id)])?:
            case Some(todo):
                return todo
            case None:
                return Err(http.Error(404, "todo not found"))

    @http.post("/todos", status=201)
    def create(self, body: NewTodo) -> Result[Todo]:
        sql = "insert into todos (title, done) values ($1, false) returning id, title, done"
        return postgres.query_one_as[Todo](self.db, sql, [body.title])?.unwrap()

def main():
    db = postgres.connect(os.getenv("DATABASE_URL").unwrap()).unwrap()
    server = http.Server()
    server.mount(App(db))
    server.listen_tls(":8443", "cert.pem", "key.pem").unwrap()
```

The compiler checks the routes against the handlers. `id` must parse as an `int`, or the request gets a 400; the body must decode as a `NewTodo`. Errors become responses:

- an error from the database becomes a 500;
- `http.Error(404, ...)` becomes a 404;
- a panic in a handler becomes a 500, and the server carries on.

Tasks share data only in forms the compiler can prove safe:

```python
@frozen
struct Job:
    id: int
    value: int

def worker(jobs: Channel[Job], results: Channel[int]):
    for job in jobs:
        results.send(job.value * job.value)

def main():
    jobs = Channel[Job](16)
    results = Channel[int](16)
    workers = [spawn worker(jobs, results) for _ in range(4)]
    ...
```

`Job` is `@frozen`, so it can cross between tasks. Passing a mutable struct or a plain `list` to `spawn` is a compile error that suggests `@frozen`, `Mutex(...)` or an immutable `tuple`.

## Features

[Weby by example](docs/examples.md) shows each of these in a runnable program.

**The language**

- Python's syntax for the parts it shares: indentation, `if`/`elif`/`else`, `for`, `while`, comprehensions, f-strings with Python's format specs, slicing, tuple unpacking, default and keyword arguments, lambdas and nested functions.
- Static types with local inference. `int` (64-bit, wrapping), `float`, `bool`, `str`, `bytes`, sized numbers (`i32`, `u8`, `f32`, ...), `list`, `dict`, tuples, and the immutable `tuple[T, ...]` and `frozendict[K, V]`.
- `struct`, `enum` with data in its variants, `impl` blocks, and traits with default methods, required traits and trait objects. Operators are traits.
- `@derive(Eq, Hash, Ord, Display, Copy, Json)` writes the standard implementations; `@frozen` makes a struct immutable.
- Generic functions, structs, enums and `impl` blocks, with bounds (`T: Ord`) and inferred type arguments, compiled once per type.
- Optional types (`T?`) instead of a `None` that fits anywhere, narrowed by `if x is not None:`.
- `Result[T]` and `?` instead of exceptions; `match` with guards, alternatives and tuple patterns.
- `for` over your own types through `next` and `iter` methods.
- `with` blocks that run an `exit` method on every way out of the block.
- Modules with `import a.b as c` and `from a import x`, `pub` and `priv`; `const`; `type` aliases.
- C functions bound with `@extern("name")`.

**What the compiler catches**

- Type errors, including mixing `int` with `float`, and `if items:` (conditions must be `bool`).
- A `match` that misses a case, and `None` used where a value is needed.
- An ignored `Result`, a variable read before it is certainly assigned, a missing `return`, unreachable code.
- Mutable data shared between tasks, and a mutex's contents escaping their lock.
- Route handlers whose parameters don't fit their route.
- Python habits, with a hint: `class`, `try`, statements at the top level of a file.

Messages show the source line and usually a fix. Constructs from the design that aren't implemented yet are rejected with an error saying so.

**Concurrency**

- `spawn f(x)` starts a task and returns a `Task` to `join`.
- Tasks are fibers spread over one worker thread per core, with an epoll poller. A task waiting on the network, a channel or a lock costs no thread.
- `Channel[T]` (buffered or not, closable, iterable), `Mutex[T]`, `Atomic` and `time.sleep`.
- A panic ends only its task, and releases the locks it held.

**For web services**

- **HTTP/1.1 server:**
  - typed routes declared with `@http.get("/todos/{id}")` and friends, or registered by calling `server.get(...)`;
  - path, query, JSON body, header and cookie parameters, checked at compile time;
  - middleware, route groups, custom error mapping, graceful shutdown on SIGTERM;
  - HTTPS with `listen_tls`;
  - `server.handle(http.test_request(...))` for testing routes without a socket.
- **WebSockets:** `@http.ws("/chat/{room}")`, `for msg in ws`, sends from any task, and secure WebSockets. The runtime handles pings, fragmentation and the closing handshake, and the server passes the Autobahn suite.
- **JSON:** encoders and decoders generated by `@derive(Json)`, with `@json(name=...)`, defaults, optional fields, tagged enums and precise error paths, plus a dynamic `json.Value`. Passes JSONTestSuite.
- **HTTP client:** `http.get`, `http.post` and `http.fetch`, for `http://` and `https://`, with the Mozilla root certificates built in.
- **PostgreSQL:** a client built into the runtime, with no `libpq`:
  - a connection pool shared by every task;
  - parameters kept separate from the SQL;
  - rows decoded into `@derive(Json)` structs;
  - transactions that roll back unless committed;
  - SCRAM authentication and TLS.
- **Other standard modules:** `re` (linear-time regular expressions), `log`, `time`, `io` (files, standard input and error) and `os` (arguments, environment, exit status).

**Tooling**

- `weby test` runs `@test` functions, each in its own task, with `assert`.
- `weby fmt` formats code.
- `weby lsp` is a language server with diagnostics, hover, go to definition and formatting.
- Dependencies come from git or a local path, with a lock file.
- Panics print source locations and a backtrace. Programs carry debug line information for `gdb` and `perf`.
- Editor support:
  - a Zed extension with a Tree-sitter grammar, outline, run buttons and the language server;
  - syntax highlighting for VS Code and Vim;
  - setup instructions for Neovim and Helix ([editors/README.md](editors/README.md)).

**Not yet**

- Sets, generic traits, and methods with their own type parameters.
- Preemption of CPU-bound tasks.
- HTTP/2, WebSocket compression, and connection reuse and redirects in the HTTP client.
- Prepared-statement caching, `COPY` and `LISTEN`/`NOTIFY` in the PostgreSQL client.
- Printing variables in the debugger: debug information is line tables and function names only.
- A VS Code extension for the language server.

## Performance

| Compared with Go | Result |
|---|---|
| Plain function calls, list building and indexing, string-keyed counting, sorting with a key function (2.4x), typed JSON | Faster than Go |
| Allocation, dynamic dispatch | 1.1x to 1.7x Go's time |
| Starting tasks | About 3x Go's time |
| HTTP server, small responses | 1.5x to 1.6x Go's `net/http` throughput |
| HTTP server, a JSON list of 20 records | 0.7x to 0.8x Go's throughput |
| Idle WebSocket connections | 11 KiB each, measured with 50,000 |

Programs are 3x to 42x faster than the same programs in CPython. The numbers and how they were measured are in [bench/README.md](bench/README.md). Allocation is the remaining gap with Go; [docs/09](docs/09-performance-plan.md) is the plan for closing it, and its first phase is done.

## Building and running

Requires Rust 1.85 or newer, LLVM 19 development files, a C compiler (for linking, and for the `ring` cryptography library behind TLS), and the Boehm garbage collector library. On Debian or Ubuntu: `apt install llvm-19-dev gcc libgc1`. The path to LLVM is set in `.cargo/config.toml`; adjust it if LLVM 19 lives elsewhere. Programs do not depend on the system's OpenSSL or certificate store: TLS is `rustls`, with the Mozilla root certificates built in, and `SSL_CERT_FILE` adds more.

```bash
cargo build --workspace
```

```bash
./target/debug/weby run tests/run/hello.wb
```

`--workspace` matters: it builds the runtime library `liblang_rt.a` next to the compiler, where the compiler looks for it.

| Command | Does |
|---|---|
| `weby run file.wb` | Compile to a temporary executable and run it |
| `weby build file.wb [-o out] [--release]` | Compile to an executable |
| `weby check file.wb` | Report errors without generating code |
| `weby build --emit=tokens\|ast\|hir\|llvm-ir\|obj file.wb` | Print or write an intermediate form |
| `weby test [paths] [--filter text]` | Run the `@test` functions, each in its own task |
| `weby fmt [paths] [--check]` | Rewrite files in the standard layout, or list the ones that are not |
| `weby fetch [--update]` | Fetch the git dependencies in `weby.toml` and record their commits in `weby.lock` |
| `weby lsp` | The language server, for editors ([editors/README.md](editors/README.md)) |

Programs are built with DWARF line information and frame pointers, so `gdb`, `perf` and panic backtraces show function names and source lines. Set `WEBY_BACKTRACE=0` to leave backtraces out of panic messages.

A project can have a `weby.toml` next to its main file (or above it) naming dependencies, from git or a local path:

```toml
[package]
name = "chat"

[dependencies]
textutil = { git = "https://example.com/textutil.git", rev = "v1.2" }
shared = { path = "../shared" }
```

`import textutil.strings` then loads `strings.wb` from the dependency, and `import textutil` its `lib.wb`.

Tests:

```bash
cargo test --workspace
```

The end-to-end tests compile every program in `tests/run` and compare its output with the `# expect:` comments in the file. Each program runs in debug mode, in release mode, and with the collector running before every allocation. The programs in [docs/examples.md](docs/examples.md) are compiled too, and run where the page shows their output. The tests also check every program in `tests/ui` against the expected compiler messages in its `.stderr` file. After an intended change to a message, regenerate those files with `WEBY_BLESS=1 cargo test -p lang_cli --test programs` and review the diff.

The HTTP, HTTPS and WebSocket servers in `tests/http` are driven over real sockets. The PostgreSQL test runs only when `WEBY_TEST_DATABASE_URL` names a database it may use ([tests/db/README.md](tests/db/README.md)). The Autobahn and JSONTestSuite conformance runs are manual, because each needs a download ([tests/conformance/README.md](tests/conformance/README.md)).

## The plan

| Document | What it covers |
|---|---|
| [docs/examples.md](docs/examples.md) | Learning the language: short programs for every feature, with their output |
| [docs/00-overview.md](docs/00-overview.md) | Goals, non-goals, the plan in one page, confirmed decisions |
| [docs/01-language-design.md](docs/01-language-design.md) | Syntax and semantics, with example programs |
| [docs/02-type-system-and-traits.md](docs/02-type-system-and-traits.md) | Types, generics, traits, dispatch |
| [docs/03-compiler-architecture.md](docs/03-compiler-architecture.md) | Compiler pipeline, crate layout, LLVM code generation |
| [docs/04-runtime-and-memory.md](docs/04-runtime-and-memory.md) | Runtime library, garbage collection, concurrency |
| [docs/05-standard-library.md](docs/05-standard-library.md) | `http`, WebSockets, `json`, tasks, core library |
| [docs/06-roadmap.md](docs/06-roadmap.md) | Milestones with exit criteria |
| [docs/07-testing-and-performance.md](docs/07-testing-and-performance.md) | Test strategy, benchmarks, performance targets |
| [docs/08-decisions-and-risks.md](docs/08-decisions-and-risks.md) | Decision log with rejected alternatives, risks, open questions |
| [docs/09-performance-plan.md](docs/09-performance-plan.md) | Where the time goes today, and the phased plan to close the gaps to Go |
| [bench/README.md](bench/README.md) | Benchmarks against Go and CPython |
| [editors/README.md](editors/README.md) | Editor support and the language server |

The name "Weby" and the `.wb` extension may still change. The name is kept out of crate names and runtime symbols and lives in one small crate of constants, so it can be changed later with a short checklist ([docs/03-compiler-architecture.md](docs/03-compiler-architecture.md#renaming-the-language)).
