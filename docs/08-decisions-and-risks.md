# 08 — Decisions, risks and open questions

Each decision records what was chosen, why, what was rejected, and what evidence would reopen it.

## Decision log

### D1. The compiler is written in Rust

**Why.** Your stated preference. Enums and pattern matching fit ASTs and type representations. `inkwell` gives a typed wrapper over the LLVM C API. The same language can be used for the runtime, so there is one toolchain.

**Rejected.** C: no sum types, manual memory management in a program that is mostly tree manipulation, and the LLVM C API is used either way. C++: gives the full LLVM C++ API, at the cost of a second build system and slower iteration.

**Revisit if.** Never, realistically.

### D2. LLVM 19 through `inkwell`, pinned

**Why.** LLVM 19 is the distribution default on the development machine (Debian 13) and is already installed. Pinning one major version avoids API drift. `inkwell` selects the LLVM version with a Cargo feature.

**Rejected.** Emitting textual `.ll` and shelling out to `llc`: simple to debug and worth keeping as a `--emit=llvm-ir` output, but slow and awkward for debug info. Raw `llvm-sys`: more unsafe code for no gain. Cranelift: faster compiles, weaker optimisation, and you asked for LLVM.

**Revisit if.** `inkwell` falls behind on a feature we need (statepoints for a precise GC are the likely case); then call `llvm-sys` directly for that feature only.

**Verified at milestone 0 (2026-11-18).** `inkwell` 0.11 with the `llvm19-1-prefer-dynamic` feature builds against Debian's LLVM 19.1.7 and links the shared `libLLVM-19.so`. `llvm-sys` finds LLVM through `LLVM_SYS_191_PREFIX`, set in `.cargo/config.toml`.

### D3. Memory is managed by a tracing garbage collector; Boehm first

**Status.** Confirmed on 2026-11-18. Automatic collection is wanted; matching C or Rust speed is not a goal.

**Why a GC at all.** The readability goal rules out ownership and lifetimes. The remaining choices are tracing GC and reference counting.

**Why tracing over reference counting.**
- A web server shares read-mostly state (router, config, connection pools, caches) across every task. With atomic reference counts, every request that touches a shared object writes to its count, and those writes contend across cores. Tracing GC makes reads free.
- Reference counting leaks cycles. Python users do not expect that, because CPython has a cycle collector.
- Inserting retain and release correctly on every path (early returns, `?`, closures, temporaries) is a large correctness surface in the compiler. A tracing GC needs nothing from the compiler until we want precise scanning.
- Go, the performance reference, uses a tracing GC.

**Why Boehm (bdwgc) first.** It is conservative, so the compiler needs no stack maps, safepoints or write barriers: allocation is one runtime call. It supports threads and parallel marking. Crystal (LLVM backend, fibers, Boehm) shows the combination works for a language of this kind and performs well on web workloads.

**Rejected.** Atomic reference counting (above). Ownership and borrowing (readability). Writing our own collector for the prototype (months of work before the language can run a web server).

**What keeps the decision reversible.**
- The language never promises when memory is freed. Resource cleanup is explicit (`with`), never tied to object lifetime. No user-visible finalisers.
- All allocation goes through one runtime entry point that receives a type descriptor. The descriptor carries a pointer bitmap from day one even though Boehm ignores it.

**Revisit if.** The allocation benchmarks at the M8 gate show GC as the dominant cost, or pause times on a multi-gigabyte heap are unacceptable. The exit path is a precise collector, most likely via MMTk; see [04](04-runtime-and-memory.md#beyond-boehm).

### D4. Go-style concurrency without function colouring; OS threads first, fibers later

**Why.** `async`/`await` splits a language into two kinds of function and hurts readability. Go's model (write blocking code, the runtime multiplexes it) is the simplest thing to read. The language defines *tasks*; whether a task is an OS thread or a fiber is a runtime detail.

**Staging.** Stage 1 maps each task to an OS thread, which is a few hundred lines of runtime and enough for a working prototype. Stage 2 replaces that with M:N fibers and an epoll poller. Programs do not change, because every blocking operation already goes through the runtime.

**Rejected.** `async`/`await` on top of Tokio: the runtime would be nearly free, but function colouring leaks into the language and the compiler has to do a coroutine transform. Building on the `may` coroutine crate: tempting, but the GC must enumerate every fiber stack, so we need to own stack allocation.

**Revisit if.** Fibers plus conservative stack scanning turn out to be unreliable across worker threads. Fallback: stackless tasks for the HTTP layer only.

### D5. Errors are values (`Result`), not exceptions

**Status.** Confirmed on 2026-11-18.

**Why.** No unwinding tables, landing pads or personality functions in codegen. Error paths are visible in signatures. It interacts trivially with fibers. It matches the Rust-flavoured half of the language.

**Cost.** This is the largest visible departure from Python. `?` and implicit `Ok` wrapping keep the noise low.

**Rejected.** `try`/`except` with LLVM zero-cost exceptions: familiar, but several weeks of codegen and runtime work, and hidden control flow. Go-style `(value, err)` pairs: verbose, and easy to ignore.

**Revisit if.** You want `try`/`except` enough to pay for it. It could be added later as sugar over `Result` for a single function body without unwinding.

### D6. Structs are reference types

**Status.** Confirmed for now on 2026-11-18; you may revisit it.

**Why.** Assignment and argument passing behave as in Python: no copies, mutation is visible through every reference. One rule, no `&` or `*`.

**Cost.** One allocation per struct, pointer chasing for nested structs. Mitigated later by escape analysis (stack allocation of non-escaping structs) and, if needed, an opt-in `@value` annotation for small plain-data structs.

**Rejected.** Go's value structs plus pointers: faster by default, but introduces pointer syntax and the copy-versus-alias distinction that Python programmers trip over.

**Revisit if.** Allocation profiles of real handlers are dominated by small short-lived structs that escape analysis cannot remove.

### D7. Generics are monomorphised; a trait used as a type is dynamically dispatched

**Why.** Monomorphisation gives the fastest code and lets LLVM inline through generic containers. Using a bare trait name as a parameter type (`def f(s: UserStore)`) gives Go-interface convenience with no extra syntax. The rule is short: `[T: Trait]` is static, `x: Trait` is dynamic.

**Cost.** Larger binaries and slower compiles for generic-heavy code. Acceptable for a server language.

**Rejected.** Dynamic dispatch only (Go before 1.18): simpler compiler, slow collections. Static dispatch only: no heterogeneous collections, which service code needs.

### D8. No mid-level IR in the prototype

**Why.** With a tracing GC there is no drop or retain logic to place, so a control-flow-graph IR between the typed tree and LLVM earns little at first. Lowering goes typed HIR to LLVM IR directly, using `alloca` plus LLVM's `mem2reg`.

**Cost.** Definite-assignment analysis and match exhaustiveness run on a tree. Escape analysis, when it comes, will want a CFG.

**Revisit if.** Escape analysis, a precise GC, or yield-point insertion starts. Insert the MIR between monomorphisation and codegen at that point.

### D9. The runtime is written in Rust and exposes a C ABI

**Why.** One toolchain. Good crates exist for the pieces that are pure protocol work (HTTP parsing, routing, SHA-1, base64, later TLS). Built as a static library with `panic = "abort"`.

**Constraint that must be respected.** Boehm does not scan memory allocated by Rust's allocator. Any reference to a Weby object held by the runtime must live in GC-visible memory. See [04](04-runtime-and-memory.md#the-rust-heap-rule).

**Rejected.** C runtime: closer to Boehm, worse for everything else. Writing the runtime in Weby: the long-term direction for the library layer, impossible at the start.

### D10. The web stack is implemented in the runtime first, then migrated into Weby

**Why.** HTTP, WebSocket framing and JSON scanning can be working within weeks in Rust. Writing them in Weby needs generics, byte buffers, raw I/O and a mature optimiser. The Weby-facing API is the stable contract; what sits behind it can move.

**Cost.** Calls across the runtime boundary are not inlined. Hot paths that matter (list indexing, string length, JSON struct encoding) are emitted inline by the compiler instead.

### D11. Variables are declared by first assignment

**Status.** Confirmed on 2026-11-18.

**Why.** `x = 5` is the Python way and you asked for Python-like syntax. Scope is the enclosing function. The compiler rejects reads on any path where the variable is unassigned, rejects type changes, and warns on variables that are assigned and never read.

**Cost.** A typo in an assignment target creates a new variable. The unused-variable warning catches most of these.

**Rejected.** `let`/`var`: safer, less Python-like. Block scoping: surprising to Python users for `if`/`else` initialisation.

**Revisit if.** Typo bugs show up in real use. Adding an optional `var` keyword later is backward compatible.

### D12. Whole-program compilation into one LLVM module

**Why.** Simplest driver, best optimisation (everything is visible to the inliner), no object file format or metadata to design.

**Cost.** Compile time grows with program size. Fine for the prototype.

**Revisit if.** Clean builds of realistic services exceed a few seconds. Then: per-module codegen units compiled in parallel, with ThinLTO in release mode.

### D13. Linux x86-64 only at first

**Why.** It is the development machine and the deployment target for almost all backends. epoll, the context-switch assembly and the linker invocation are each platform specific.

**Next.** Linux aarch64 (small: new context-switch stub and target triple), then macOS (kqueue, different linker flags).

### D14. The language is called Weby, with `.wb` source files

**Status.** Chosen by you on 2026-11-18, replacing the earlier placeholders. You asked that it stay possible to change later.

**How the name stays cheap to change.** Crate names, the runtime library and runtime symbols do not contain the language name. The name, file extension, CLI name and environment variable prefix are constants in one small crate. See [03](03-compiler-architecture.md#renaming-the-language).

### D15. Static typing in Python type-hint syntax; missing or mismatched types are compile errors

**Status.** Required by you on 2026-11-18.

**What it means.**
- Types are written exactly as Python type hints: `name: type` for parameters, fields and variables, `-> type` for returns, `list[int]`, `dict[str, int]`, `tuple[int, str]`.
- Every function parameter and struct field must have a hint. A missing one is a compile error.
- Every type mismatch is a compile error. There are no runtime type errors, no `Any`, and no `isinstance`.
- Local variables are statically typed by inference from their first assignment, which is how `x = 5` stays Python-like without becoming dynamic. A hint is required only when the value does not determine the type (`items: list[int] = []`).
- Python's built-in types keep their Python names. The string type is `str`; writing `string` produces an error that says so.

**Judgement calls inside this decision.**
- A function with no `->` is typed as returning `None`, not left untyped. Confirmed on 2026-11-18.
- `T?` is the short spelling of Python's `T | None`; both are accepted.
- Function types are written `def(A) -> R` in place of Python's `Callable[[A], R]`, because it reads like the definition it describes.

**Rejected.** Gradual typing (optional hints, as in Python itself): it would need a dynamic value representation and runtime type checks, which costs the speed that static typing buys.

### D16. Integer arithmetic wraps on overflow

**Status.** Decided by you on 2026-11-18: overflow behaves as in Go.

**What it means.** `int` is 64-bit and wraps around silently in every build mode. Sized integers wrap at their own width. Debug and release builds behave identically. Integer division by zero still panics.

**Cost.** An overflow bug is not caught in testing. The compiler emits arithmetic without LLVM's no-overflow flags, which forgoes a few loop optimisations; the effect is small.

**Rejected.** Panic in debug and wrap in release (Rust's rule): catches bugs in testing but makes the two modes behave differently. Unbounded integers (Python's rule): every arithmetic operation becomes a possible allocation.

### D17. Numbers do not mix implicitly; arithmetic is defined by traits

**Status.** Decided by you on 2026-11-18.

**What it means.** Literals adapt to the type they are used with (`1 + 2.5` works). Variables do not: `int + float` is a compile error and `float(n)` is the fix. Addition is defined by the built-in trait `AddNumber`, with `SubNumber` and `MulNumber` alongside, and the number types implement them. See [02](02-type-system-and-traits.md#arithmetic-traits).

**How I read "the add_number trait".**
- It is spelled `AddNumber`, since trait names are `CamelCase`.
- Both operands have the same type (`other: Self`). That is consistent with not mixing `int` and `float`.
- User structs may implement it, which gives them `+`.

**If you meant something else.** If the trait was meant to define addition between *different* number types (so that `int + float` works through it), the trait takes the right-hand type as a parameter instead, and the "variables need `float(x)`" rule goes away. That is a contained change to [02](02-type-system-and-traits.md#arithmetic-traits).

### D18. Visibility uses `pub` and `priv`

**Status.** Decided by you on 2026-11-18, in place of the leading-underscore convention.

**What it means.** `pub` exports a top-level declaration from its file; without it the declaration is private to the file. `priv` hides a field, method or enum variant inside a type; without it the member is visible wherever the type is. See [01](01-language-design.md#visibility).

**Judgement call.** You asked for both keywords and did not say what the default is. I gave each keyword one job, chosen so that typical code needs the fewest markers: a file's public surface is opt-in, while a public struct's fields and methods do not each need a keyword.

**Rejected.** Private by default everywhere (Rust): `pub` on nearly every field and method of a data type. Public by default everywhere (Python): a file's public surface is accidental, and unused-code warnings become unreliable.

### D19. Route annotations and typed handler parameters, in the prototype

**Status.** Decided by you on 2026-11-18. An earlier version of this plan deferred both until after the prototype.

**What it means.** `@http.get("/todos/{id}")` on a function or method declares a route. Its parameters are bound from the path, query string and JSON body according to their names and types, and its return value is encoded according to its type. All of it is resolved at compile time into the call-style API. See [05](05-standard-library.md#http).

**Why it fits a compiled language after all.** These are a fixed set of compiler-known annotations, not general decorators. The compiler sees every handler signature, so it can generate the binding code and report a mismatch between a path and a signature as a compile error, which a dynamic framework can only report when the request arrives.

**Design choices.**
- No global application object. Routes are methods on a struct (or plain functions) and are attached with `server.mount(value)`. Shared state is `self`, which avoids mutable globals.
- The call-style API remains the foundation and stays public.

**Cost.** Milestone 8 grows. The work is confined to the compiler's derivation pass and adds nothing to the runtime.

**Risk.** Pressure to keep adding binding rules (headers, cookies, forms, files, dependency injection). The rule table in [05](05-standard-library.md#how-handler-parameters-are-bound) is the fence; anything else goes through `http.Request`.

### D20. The compiler prevents data races at task boundaries

**Status.** Decided by you on 2026-11-18: have the compiler do this if possible.

**What is possible.** A complete guarantee needs the compiler to track every reference (ownership and borrowing, as in Rust), which contradicts the readability goal. What fits the language is a check on what may cross between tasks: only immutable values, `@frozen` structs and synchronised types. That removes the memory-corrupting races and the common logic races at no run-time cost. See [04](04-runtime-and-memory.md#data-race-prevention).

**What remains uncovered.** A callee that stores a lock-guarded value somewhere that outlives the lock; C code; logic races across separate lock acquisitions. A race-detecting build mode is planned as the backstop.

**Cost.** One new annotation (`@frozen`), one compiler-computed trait (`Shareable`), immutable collection types, and a restriction that will occasionally reject a correct program (handing a mutable value to another task and never touching it again).

**Rejected.** Go's stance of leaving it to the programmer: simplest, and what the previous version of this plan assumed. Full ownership tracking: sound, and no longer a Python-like language. Actors only, with no shared memory at all: sound, but a poor fit for shared caches and connection pools.

## Risks

| # | Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|---|
| R1 | Boehm cannot keep up with Go on allocation-heavy request handling | High | Medium | Use pointer-free allocation for strings and byte buffers; reduce allocation in the HTTP path; escape analysis; precise GC as the planned exit. Measured at the M8 gate. |
| R2 | A GC reference is held only in Rust-allocated memory and gets collected | Medium | High (memory corruption) | The Rust heap rule in [04](04-runtime-and-memory.md#the-rust-heap-rule); a single handle-table API; a GC stress mode (collect on every allocation) run in CI. |
| R3 | Fibers plus conservative stack scanning across worker threads is fragile | Medium | High | Own the fiber implementation; register stacks through the documented Boehm hook; stage 2 is after the prototype, so the prototype does not depend on it. |
| R4 | Scope creep in the language (features before the web stack works) | High | High | The roadmap's exit criteria are programs, not features. Anything not needed by a milestone's demo waits. |
| R5 | Indentation lexing and f-strings produce confusing errors | Medium | Medium | Dedicated diagnostics tests from milestone 1; tabs are rejected outright. |
| R6 | Generic syntax `f[T](x)` is ambiguous with indexing | Certain | Low | Parse as an index expression; the type checker reinterprets it when the base names a generic. Documented in [03](03-compiler-architecture.md#parser). |
| R7 | Trait system grows into Rust's (associated types, blanket impls, specialisation) | Medium | Medium | Explicit feature fence in [02](02-type-system-and-traits.md#what-the-trait-system-deliberately-lacks). |
| R8 | A panic in one request handler takes down the server | Certain in stage 1 | Medium | Stage 1 documents it; most failures are `Result`s. Task-scoped panics arrive with fibers ([04](04-runtime-and-memory.md#panics)). |
| R9 | `inkwell` lags LLVM or lacks an API | Low | Low | LLVM is pinned; `llvm-sys` is available underneath. |
| R10 | The sharing rules reject too much, and programmers wrap everything in `Mutex` to get past them | Medium | Medium | Diagnostics that name the fix; immutable collections in M6; review the rules against the M8 and M9 services, which are the realistic test. See D20 for what the rules do not cover |
| R11 | Route binding rules grow into a framework inside the compiler | Medium | Medium | The fixed rule table in [05](05-standard-library.md#how-handler-parameters-are-bound); everything else goes through `http.Request` |
| R12 | Milestone 8 is too large to finish in one piece | Medium | Low | It has two stages; stage A (call style) is shippable alone |

## Open questions

Nothing here blocks the start of implementation. The six questions that used to be on this list were answered on 2026-11-18 and are recorded as D16 to D20 and in D15.

Design details to settle when their milestone starts:

1. **Immutable collections** (M6). Which of `tuple[T, ...]`, `frozenset[T]` and a frozen `dict` are needed, and what the frozen `dict` is called.
2. **`select`** (after M6). Syntax for waiting on several channels at once.
3. **Cancellation and deadlines** (after M8). How a request's deadline reaches the waiting operations inside its handler.
4. **Headers and cookies in typed handlers** (after M8). Whether they get a binding rule or stay on `http.Request`.
5. **Moving a mutable value into another task** (post-prototype). Whether a limited form of ownership transfer is worth its complexity.
6. **Arithmetic traits for division** (M4). Whether `/`, `//` and `%` need trait forms for generic code.
