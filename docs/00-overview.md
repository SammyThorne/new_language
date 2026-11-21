# 00 — Overview

## What we are building

Viper is a statically typed, ahead-of-time compiled language for writing web backends. It reads like Python (indentation, colons, `def`, no braces or semicolons), organises code like Rust (structs, traits, `impl` blocks, enums, no inheritance), and runs like Go (native binary, garbage collected, cheap concurrent tasks, HTTP in the standard library).

The compiler frontend is written in Rust and emits LLVM IR. A small runtime library, also in Rust, is linked into every program.

## A taste

The snippets in these documents are Viper source, shown to pin down the design. They are not implementation code.

```python
import http

@derive(Json)
@frozen
struct User:
    id: int
    name: str
    email: str?

@derive(Json)
@frozen
struct NewUser:
    name: str
    email: str?

@frozen
struct Api:
    users: Mutex[dict[int, User]]
    next_id: Atomic[int]

impl Api:
    @http.get("/users/{id}")
    def get_user(self, id: int) -> Result[User]:
        with self.users.lock() as users:
            match users.get(id):
                case Some(user):
                    return user
                case None:
                    return Err(http.Error(404, "user not found"))

    @http.post("/users", status=201)
    def create_user(self, body: NewUser) -> User:
        user = User(id=self.next_id.increment(), name=body.name, email=body.email)
        with self.users.lock() as users:
            users[user.id] = user
        return user

def main():
    api = Api(users=Mutex({}), next_id=Atomic(0))
    server = http.Server()
    server.mount(api)
    server.listen(":8080")
```

Everything in that example is checked at compile time: that `{id}` in the path has a matching `id: int` parameter, that `NewUser` can be decoded from a JSON body and `User` encoded to one, and that `Api` is safe to share between the tasks serving requests.

## Goals, in priority order

1. **Readability.** A Python programmer should be able to read a Viper service on day one. One obvious way to do each thing; little punctuation; no lifetime or ownership annotations.
2. **Speed.** Much faster than CPython from the first milestone; within reach of Go on HTTP and JSON workloads by the end of the roadmap.
3. **Web backend batteries.** REST routing, WebSockets, JSON and concurrency are in the standard library and designed together.
4. **Trait polymorphism.** Structs plus traits are the only way to get polymorphic behaviour. There are no classes and no inheritance.
5. **Small.** A language one person can hold in their head and one small team can implement.

## Non-goals for the prototype

- Inheritance, classes, metaclasses, runtime reflection, `eval`.
- Exceptions with stack unwinding (errors are values, see [01](01-language-design.md#errors)).
- `async`/`await`. Concurrency is Go-style: ordinary blocking-looking code on cheap tasks.
- A borrow checker or manual memory management.
- Macros and user-defined decorators. Route annotations such as `@http.get` are built into the compiler; users cannot define new ones.
- Package manager, formatter, language server, debugger integration (planned after the prototype).
- Windows and macOS. Linux x86-64 first.
- TLS, HTTP/2. The prototype sits behind a reverse proxy.

## The plan on one page

```
 .vi source
     │
     ▼
 ┌────────┐  tokens with    ┌────────┐  AST   ┌──────────┐  typed HIR  ┌────────────────┐
 │ Lexer  │ ──────────────▶ │ Parser │ ─────▶ │ Resolve  │ ──────────▶ │ Monomorphise   │
 │        │  INDENT/DEDENT  │        │        │ + Typeck │             │ (generics out) │
 └────────┘                 └────────┘        └──────────┘             └───────┬────────┘
                                                                               │
                                                                               ▼
                                 native binary ◀── system linker ◀── object ◀─ LLVM codegen
                                                        ▲                      (inkwell)
                                                        │
                                              liblang_rt.a  (Rust runtime: GC glue,
                                              strings, collections, tasks, sockets,
                                              HTTP, WebSocket, JSON scanning)
```

| Area | Choice | Detail |
|---|---|---|
| Compiler language | Rust | [03](03-compiler-architecture.md) |
| Backend | LLVM 19 through the `inkwell` crate | [03](03-compiler-architecture.md#llvm-code-generation) |
| Typing | Static, nominal, local inference, annotated signatures | [02](02-type-system-and-traits.md) |
| Polymorphism | Traits; static dispatch through generics, dynamic dispatch when a trait is used as a type | [02](02-type-system-and-traits.md#traits) |
| Generics | Monomorphised | [02](02-type-system-and-traits.md#generics) |
| Memory | Tracing garbage collector. Boehm (bdwgc) first, behind a swappable allocation interface | [04](04-runtime-and-memory.md#garbage-collection) |
| Concurrency | `spawn` tasks, channels, mutexes. OS threads first, M:N fibers with an epoll poller second, same source code for both | [04](04-runtime-and-memory.md#concurrency) |
| Errors | `Result[T]` values with `?` propagation; panics for bugs | [01](01-language-design.md#errors) |
| Web stack | HTTP/1.1 server, router, WebSockets and JSON in the standard library, implemented in the runtime first | [05](05-standard-library.md) |
| Modules | One file is one module; `import name` | [01](01-language-design.md#modules) |
| Visibility | Declarations are private to their file unless `pub`; members of a type are visible with it unless `priv` | [01](01-language-design.md#modules) |
| Routes | `@http.get("/path/{id}")` annotations with typed parameters, compiled down to call-style registration | [05](05-standard-library.md#http) |
| Data races | Compile-time check on what may be shared between tasks | [04](04-runtime-and-memory.md#data-race-prevention) |
| Integer overflow | Wraps, as in Go | [08](08-decisions-and-risks.md) D16 |

## Honest expectations on speed

- **CPU-bound code** (loops, arithmetic, struct field access) goes through the same LLVM optimiser as Rust and C. Expect Go-level speed early.
- **Allocation-heavy code** will trail Go while the collector is Boehm. Go's allocator and collector are tuned for exactly this workload and a conservative collector is not. This is the main known gap and has a planned exit ([04](04-runtime-and-memory.md#beyond-boehm)).
- **HTTP throughput** depends on the runtime, not the language. Thread-per-connection (prototype) is far ahead of Python frameworks and behind Go at high connection counts. Fibers plus epoll (milestone 10) is what closes the gap.

"Faster than Python" is a low-risk target. "As fast as Go" is achievable for most backend workloads and is a post-prototype goal that the benchmark suite in [07](07-testing-and-performance.md) tracks from the start.

## Local toolchain

Checked on the development machine on 2026-11-18:

| Tool | Status |
|---|---|
| Rust 1.100 / Cargo | installed |
| LLVM 19 (`llvm-config-19`) and Clang 19.1.7 | installed |
| GCC 14.2 (`cc`, used as the linker driver) | installed |
| Boehm GC headers (`libgc-dev`) | not installed; needed from milestone 2, or vendored |
| Go 1.27.4 | installed in `~/.local/go` (not on `PATH`); used only as a benchmark baseline |
| Python 3.13 | installed; benchmark baseline |

## Confirmed decisions

Confirmed by you on 2026-11-18. Reasoning and rejected alternatives are in [08](08-decisions-and-risks.md).

| # | Decision | Record |
|---|---|---|
| 1 | The language is called **Viper**, source files end in `.vi`, the CLI is `viper`. Still changeable: a rename touches one small crate and the docs ([03](03-compiler-architecture.md#renaming-the-language)) | D14 |
| 2 | Memory is managed by an automatic garbage collector. Matching C or Rust is not a goal | D3 |
| 3 | Variables are declared by first assignment (`x = 5`), with no `let` or `var` | D11 |
| 4 | Errors are `Result` values propagated with `?`, not `try`/`except` | D5 |
| 5 | Structs are reference types, as Python objects are. May be revisited | D6 |
| 6 | Static typing written as Python type hints (`def test(a: int) -> str`). A missing or mismatched type is a compile error ([02](02-type-system-and-traits.md#typing-rules)) | D15 |
| 7 | Python's built-in types are included under their Python names; structs are the user-defined types ([01](01-language-design.md#built-in-types)) | D15 |
| 8 | Integer arithmetic wraps on overflow, as in Go | D16 |
| 9 | `int` and `float` do not mix implicitly; literals adapt. Arithmetic is defined by the `AddNumber` family of traits ([02](02-type-system-and-traits.md#arithmetic-traits)) | D17 |
| 10 | Visibility uses `pub` and `priv` keywords ([01](01-language-design.md#modules)) | D18 |
| 11 | Routes are declared with `@http.get(...)`-style annotations, and handler parameters are typed and decoded from the request. Both are in the prototype ([05](05-standard-library.md#http)) | D19 |
| 12 | The compiler prevents data races where it can, by checking what crosses between tasks ([04](04-runtime-and-memory.md#data-race-prevention)) | D20 |

No open question blocks the start of implementation. The remaining ones are listed in [08](08-decisions-and-risks.md#open-questions).
