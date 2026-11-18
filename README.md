# Weby

A compiled, Python-shaped language for web backends: indentation syntax, structs and traits instead of classes, an LLVM backend, and REST, WebSocket, JSON and concurrency support in the standard library.

## Status

Milestones 0 to 5 of the [roadmap](docs/06-roadmap.md#progress) are done. Source is lexed, parsed, type-checked and compiled to native code through LLVM 19, then linked with a Rust runtime that uses the Boehm garbage collector.

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

| Works now | Not yet |
|---|---|
| Functions with type hints, default and keyword arguments, `if`/`elif`/`else`, `while`, `for`, `break`, `continue`, `pass` | Tasks, JSON, HTTP, WebSockets (milestones 6 to 9) |
| `int` (wrapping), `float` (printed exactly as Python prints it), `bool`, `str`, sized numbers (`i32`, `u8`, `f32`, ...) | |
| Modules: `import a.b as c`, `from a import x`, `pub` and `priv`; the `io` and `os` standard modules (files, stdin, stderr, arguments, environment) | |
| `with` blocks that close files on every way out; C functions bound with `@extern("name")` | |
| Structs, enums, methods, traits with default methods, trait objects | Sets, generic traits, methods with their own type parameters |
| Generic functions, structs, enums and `impl` blocks with bounds (`T: Ord`), inferred type arguments | |
| Lambdas, nested functions with captured variables, functions and bound methods as values | |
| List and dict comprehensions; `for` over user iterators (`next`) and iterables (`iter`) | |
| `match` with exhaustiveness checking; `Result`, `?`, `int.parse`; narrowing after `x is not None` | |
| `@derive(Eq, Hash, Display, Copy, Ord)`, `@frozen`, operator traits | |
| `list`, `dict`, tuples, optionals (`T?`), f-strings with Python's format specs | |
| `min`, `max`, `sum`, `sorted` and `sort` with `key=` and `reverse=`, `enumerate`, `zip`, `any`, `all` | |
| Panics with source locations; diagnostics with source snippets | |

Constructs from the design that are not implemented are rejected with an error saying they are not supported yet.

Compiled programs run between 1.2x and 1.7x Go's time on allocation, collections, strings and dynamic dispatch, and faster than Go on plain function calls and on sorting with a key function. Against CPython they are 11x to 35x faster, except about 2x on string-and-dict-heavy code; see [bench/README.md](bench/README.md).

## Building and running

Requires Rust 1.85 or newer, LLVM 19 development files, a C compiler for linking, and the Boehm garbage collector library. On Debian or Ubuntu: `apt install llvm-19-dev gcc libgc1`. The path to LLVM is set in `.cargo/config.toml`; adjust it if LLVM 19 lives elsewhere.

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

Tests:

```bash
cargo test --workspace
```

The end-to-end tests compile every program in `tests/run` (in debug and release mode) and compare its output with the `# expect:` comments in the file, and check every program in `tests/ui` against the expected compiler messages in its `.stderr` file. After an intended change to a message, regenerate those files with `WEBY_BLESS=1 cargo test -p lang_cli --test programs` and review the diff.

## The plan

| Document | What it covers |
|---|---|
| [docs/00-overview.md](docs/00-overview.md) | Goals, non-goals, the plan in one page, confirmed decisions |
| [docs/01-language-design.md](docs/01-language-design.md) | Syntax and semantics, with example programs |
| [docs/02-type-system-and-traits.md](docs/02-type-system-and-traits.md) | Types, generics, traits, dispatch |
| [docs/03-compiler-architecture.md](docs/03-compiler-architecture.md) | Compiler pipeline, crate layout, LLVM code generation |
| [docs/04-runtime-and-memory.md](docs/04-runtime-and-memory.md) | Runtime library, garbage collection, concurrency |
| [docs/05-standard-library.md](docs/05-standard-library.md) | `http`, WebSockets, `json`, tasks, core library |
| [docs/06-roadmap.md](docs/06-roadmap.md) | Milestones with exit criteria |
| [docs/07-testing-and-performance.md](docs/07-testing-and-performance.md) | Test strategy, benchmarks, performance targets |
| [docs/08-decisions-and-risks.md](docs/08-decisions-and-risks.md) | Decision log with rejected alternatives, risks, open questions |

The name "Weby" and the `.wb` extension may still change. The name is kept out of crate names and runtime symbols and lives in one small crate of constants, so it can be changed later with a short checklist ([docs/03-compiler-architecture.md](docs/03-compiler-architecture.md#renaming-the-language)).
