# Release notes

Weby is a compiled, Python-shaped language for web backends. `weby --version` reports 1.1.0: the first release, 1.0.0, with the API documentation of [1.1.0](#new-in-110) added. This page says what the language has, what it does not, and how to install it. The state of each part, with the reasoning behind the choices, is in the [roadmap](06-roadmap.md) and the [decision log](08-decisions-and-risks.md); [Weby by example](examples.md) teaches the language, and every program in it is compiled and run by the test suite.

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

**Tooling.** `weby run`, `build`, `check`, `test` (for `@test` functions), `fmt`, `fetch`, `explain` (for an error code) and `lsp`. The language server gives diagnostics, hover, go to definition, completion and formatting. Editor support for VS Code, Vim, Neovim, Helix and Zed is in `editors/`. The messages point at the source, suggest the nearest name, and every code has an entry in the [error reference](errors.md).

**Performance.** On this repository's benchmarks every program runs faster than Go's version (between 1.1 and 3.0 times), and the HTTP endpoints serve 1.8 to 3.1 times Go's request rate at 256 connections with about 40% of its CPU per request. Idle WebSocket connections cost about 19 KiB each. A small program's executable is 0.5 MB and the benchmark web server's 1.3 MB. The numbers, how they were measured and what limits them are in [web-performance.md](web-performance.md) and [bench/README.md](../bench/README.md). `bench/run.py --check` and `bench/http/counts.py --check` fail on a regression.

## Since 1.1.0: signing in with Google, and production web features

Not yet in a numbered release; `weby --version` still says 1.1.0.

**Logs and requests** ([production web plan](14-production-web-plan.md), milestone 1, D105):
- `log` functions take `fields` (`log.info("saved", {"order": id})`), written as `name=value`, or as one JSON object per line after `log.set_format("json")` or with the `LOG_FORMAT` variable. `log.with_fields(...)` gives a logger with fields of its own.
- `http.request_log()` gives each request an ID (the client's `X-Request-Id` or a new one), returns it, logs one line per request, and adds the ID to every line logged while the request is handled, including from tasks it spawns.
- While a server runs, log lines to a pipe or file are written in blocks rather than one system call each.
- `req.client_ip()` with `server.trusted_proxies`, for servers behind a proxy; `req.body()` as `bytes`; `http.binary(...)` for responses of any bytes; `os.setenv`.

**CORS** (milestone 2, D106): `http.cors(origins=[...])` lets pages on other origins call the server from a browser. It answers preflights, adds the headers to responses (error responses too), supports credentials, exposed headers and a function that decides origins, and works on a group of routes. A lambda can now be passed where an optional function is expected.

**Static files** (milestone 3, D107): `server.static("/assets", "public")` serves a directory from the runtime, binary-safe, with content types, `ETag` and `Last-Modified` (304s and 412s), `Cache-Control` (`max_age`, `immutable`), single ranges (206, 416, `If-Range`), pre-compressed `.br` and `.gz` files, directory indexes, and large files sent in pieces so memory stays flat. `fallback="index.html"` serves a single-page app. Paths that try to leave the directory (`..` in any spelling, encoded separators, symbolic links) and dotfiles are 404s.

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
- completion in the language server, a warning for unused imports, `weby explain`, and executables four to eight times smaller.

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
- `weby fmt` keeps your line breaks; it does not fit lines to a width (D68). The language server checks the whole program on each change (D70).
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

The first builds the compiler and the runtime library; the second puts the `weby` command in `~/.local/bin` (`PREFIX=... make install` for somewhere else). Then:

```bash
weby run hello.wb
```
