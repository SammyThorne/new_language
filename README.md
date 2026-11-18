# Weby

A compiled, Python-shaped language for web backends: indentation syntax, structs and traits instead of classes, an LLVM backend, and REST, WebSocket, JSON and concurrency support in the standard library.

Nothing is implemented yet. This repository currently holds the implementation plan.

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
