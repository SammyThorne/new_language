# Weby

A compiled, Python-shaped language for web backends: indentation syntax, structs and traits instead of classes, an LLVM backend, and REST, WebSocket, JSON and concurrency support in the standard library.

## Status

Milestones 0 to 2 of the [roadmap](docs/06-roadmap.md#progress) are done. Source is lexed, parsed, type-checked and compiled to native code through LLVM 19, then linked with a Rust runtime that uses the Boehm garbage collector.

```python
struct WordCount:
    word: str
    count: int

def top(counts: dict[str, int]) -> WordCount?:
    best: WordCount? = None
    for word, count in counts.items():
        if best is None or count > best.unwrap().count:
            best = WordCount(word, count)
    return best

def main():
    counts: dict[str, int] = {}
    for word in "the cat and the hat".split():
        counts[word] = counts.get_or(word, 0) + 1
    winner = top(counts).unwrap()
    print(f"most common: {winner.word} ({winner.count})")
```

| Works now | Not yet |
|---|---|
| Functions with type hints, `if`/`elif`/`else`, `while`, `for`, `break`, `continue` | Traits, enums, `match`, `Result` and `?` (milestone 3) |
| `int` (wrapping), `float` (printed exactly as Python prints it), `bool`, `str` | Generics, lambdas, closures (milestone 4) |
| Structs with defaults, methods and static methods | Modules and imports (milestone 5) |
| `list`, `dict`, tuples, optionals (`T?`) with their usual methods | Tasks, JSON, HTTP, WebSockets (milestones 6 to 9) |
| f-strings with Python's format specs | Sets, printing structs |
| Panics with source locations; diagnostics with source snippets | |

Constructs from the design that are not implemented are rejected with an error saying they are not supported yet.

Compiled programs run 20x to 35x faster than CPython on calls, loops and allocation, and about 2x on string-and-dict-heavy code; see [bench/README.md](bench/README.md).

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
