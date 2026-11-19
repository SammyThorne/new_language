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

### D21. The prototype links the system's Boehm GC

**Status.** Decided while implementing milestone 2, 2026-11-18.

**What it means.** Programs link `libgc.so.1`, which Debian ships in `libgc1`, by its full file name (`-l:libgc.so.1`). The runtime declares the handful of collector functions it calls, so building needs no headers or `-dev` package. A compiled program therefore needs `libgc1` installed where it runs.

**Why.** The development machine had the library but not the headers, and there was no way to install packages. This removes a download and a C build from the build.

**Revisit.** Before the first release: vendor bdwgc and link it statically, as D3 already plans, so binaries are self-contained.

### D22. Strings are indexed by byte offset

**Status.** Decided while implementing milestone 2, 2026-11-18, following docs/01.

**What it means.** `len(s)` is the length in bytes, slicing uses byte offsets, and `s.find(x)` returns a byte offset. Slicing in the middle of a character panics. `for c in s` yields characters. Indexing a single position (`s[i]`) is a compile error that suggests slicing or iterating.

**Why.** Strings are UTF-8, so byte offsets are constant-time and character offsets are not. This matches Go and Rust, and differs from Python, whose `len` counts code points. For ASCII text, which most protocol data is, the two agree.

### D23. Float division by zero follows IEEE 754; integer division by zero panics

**Status.** Decided while implementing milestone 1, 2026-11-18.

**What it means.** `1.0 / 0.0` is `inf` and `0.0 / 0.0` is `nan`, as in Go and Rust. Integer `//`, `%` and `/` by zero panic. Python raises an error in both cases.

**Why.** Floats already have values for these results, and checking every float division costs a branch in numeric code. Integers have no such value.

### D24. A panic prints its location and exits with status 101

**Status.** Decided while implementing milestone 1, 2026-11-18.

**What it means.** A panic flushes standard output, prints `panic: <message>` and ` --> file:line:col` to standard error, and exits with status 101, the same status Rust uses. Panics come from bugs: an index out of range, `unwrap()` on `None`, a missing dict key, division by zero, a negative shift count, `int()` of a non-finite float, or an explicit `panic("...")`.

### D25. Dict keys use a seeded fast hash, not SipHash

**Status.** Decided while implementing milestone 2, 2026-11-18.

**What it means.** String keys are hashed with a wyhash-style function and a random per-process seed, the approach Go's maps take. The seed keeps hash values unpredictable from outside, so request data cannot be crafted to collide. The function is not cryptographic.

**Why.** It is several times faster than SipHash on the short keys that dominate in practice. Python uses SipHash but caches each string's hash; the compiled language cannot cache, because strings are plain pointer-and-length values.

**Revisit if.** A collision attack against seeded non-cryptographic hashing becomes practical, as it would for Go too.

### D26. The built-in traits live in a prelude written in the language

**Status.** Decided while implementing milestone 3, 2026-11-18.

**What it means.** `Display`, `Eq`, `Hash`, `Copy`, `Error`, `AddNumber`, `SubNumber`, `MulNumber` and the struct `ParseError` are declared in `crates/lang_sema/src/prelude.lang` and compiled with every program. Their method names are `to_str`, `eq`, `hash`, `copy`, `message`, `add`, `sub` and `mul`. User code implements them like any trait: `impl Display for Point: def to_str(self) -> str: ...`.

**Why.** No special cases in the type checker for how these traits are declared, implemented or checked; the compiler only attaches meaning (printing, `==`, dict keys, `?`, operators). The prelude is also the first code in the language that the compiler itself depends on.

### D27. Enum values are immutable heap objects

**Status.** Decided while implementing milestone 3, 2026-11-18.

**What it means.** An enum value is a pointer to an object holding its descriptor, its variant's tag and its fields. Variants without fields are static constants, so they cost nothing to create. Fields cannot be assigned, and `is` does not apply to enums: they are values, compared with `==` when they derive `Eq`.

**Why.** docs/02 allowed the compiler to box enums. Boxing every enum keeps one representation, lets an enum be a trait object or the target of `Option` at no cost, and makes recursive enums (an expression tree) need nothing special.

**Cost.** One allocation per value of a variant with fields. Escape analysis (docs/07, optimisation backlog) is the remedy if it shows up in profiles.

### D28. Variants can be named with or without their enum

**Status.** Decided while implementing milestone 3, 2026-11-18.

**What it means.** `Shape.Circle(1.0)` always works. `Circle(1.0)` works when only one enum has a variant called `Circle`; otherwise the compiler asks for the qualified form. In a `case`, a bare name is looked up among the variants of the value being matched, so it is never ambiguous there.

**Why.** docs/01's examples use bare names (`case Text(body):`, `Err(NotFound("user"))`), which reads best; qualification resolves the rare clash.

### D29. `pass` is a keyword

**Status.** Added while implementing milestone 3, 2026-11-18.

**Why.** A block cannot be empty, so `impl Greeter for Person:` had no way to say "use every default". `pass` is Python's answer and is what Python programmers will type. It is allowed wherever a statement is, and in `struct`, `enum`, `trait` and `impl` bodies.

### D30. Small conveniences around `Result`

**Status.** Decided while implementing milestone 3, 2026-11-18.

- A function returning `Result[None]` succeeds by reaching its end or with a bare `return`, as a Python function does.
- `Ok(x)` with no expected type is a `Result[T]` (errors of any `Error` type). `Err(e)` needs an expected type, since the success type cannot be guessed.
- `int.parse` and `float.parse` return `Result[int, ParseError]`; `?` converts a `ParseError` to `Error` where needed.
- `unwrap()` on an error panics with the error's `message()`.
- `Trait.method(value, ...)` calls a specific trait's method when two traits give a type methods of the same name.

### D31. `match` is compiled as one test per case, in order

**Status.** Decided while implementing milestone 3, 2026-11-18. The roadmap asked for decision-tree code generation.

**What it means.** Each case's pattern becomes a chain of tests that falls through to the next case on the first failure. Exhaustiveness and unreachable cases are still checked with the full pattern-matrix algorithm, so the language is unaffected.

**Why.** Simpler, and fast for the matches programs actually write, which test one tag and a few fields. A decision tree avoids re-testing the same tag across many cases.

**Revisit if.** A benchmark of `match`-heavy code (an interpreter loop, a protocol parser) shows the repeated tests in a profile.

### D32. Generics are monomorphised, and each template is checked once against its bounds

A generic function, struct, enum or `impl` block is checked once with its type parameters as opaque types that only support what their bounds provide: `T: Ord` allows `<`, `T: Display` allows printing, `T: Number` allows `+`, `-` and `*`. Errors are reported from that check. Each use with concrete types then creates an instance, checked again without reporting and compiled like hand-written code, so generic code runs as fast as code written for one type. Type arguments are inferred from the arguments of a call (lambdas last, once their parameter types are known) and from the expected type, or written out: `Stack[int].new()`, `first_or[str]([], "")`. A derived trait on a generic type applies to an instance when every type argument has the trait, as if each parameter had that bound. Built-in types implement the prelude traits that describe what they support (`int` is `Number`, `Ord`, `Hash`, `Display`), so `largest([3, 1, 2])` works with `T: Ord`.

### D33. Methods do not have their own type parameters yet

`impl[T] Stack[T]:` gives methods the type's parameters, but a method cannot add one of its own (`def map[U](self, ...)`). Top-level generic functions cover the same needs. Allowing it is a small extension of the same machinery, deferred until a use needs it.

### D34. Iteration is by method names, not by generic traits

`for x in value` works when the value's type has `def next(self) -> T?` (an iterator) or `def iter(self)` returning an iterator. The plan named `Iterable[T]` and `Iterator[T]` traits, but traits cannot take type parameters yet. The method protocol gives the same static dispatch and the same syntax for users; it can become a pair of traits when generic traits arrive, without changing user code.

### D35. Closures capture variables by reference and cannot reassign them

A lambda or nested `def` sees the enclosing function's variables as they are when it runs, as in Python: a captured variable lives in a heap cell shared by the function and its closures. A closure may read captured variables and change the objects they refer to, but assigning to one is an error (E0332), where Python would need `nonlocal`. This keeps closures safe to hand to tasks later (D20) and removes a classic source of confusion. Lambdas take their parameter types from the function type expected where they appear; a lambda with parameters and no such context is an error that suggests `def`.

### D36. Function values are a code pointer and an environment

A value of type `def(A) -> R` is two words: code that takes the environment first, then the arguments. Named functions and bound methods (`staff[0].summary`) used as values get small adapter functions, so every function value is called the same way. Built-in functions such as `len` can be values where a function type is expected (`sorted(words, key=len)`).

### D37. Flow narrowing of optional variables

After `if x is not None:` (or `is None` with an early return), and on the right of `x is not None and ...`, a local `x: T?` reads as a `T`. Narrowing ends where `x` may be assigned `None` again: at an assignment of an optional value, and for the whole of a loop or `match` that assigns it. Only local variables narrow, not fields, since another reference could change a field. `x.unwrap()` still works on a narrowed `x`.

### D38. `with` uses methods named `enter` and `exit`

`with value as name:` works when the value's type has `def exit(self)`, which runs however the block is left: at its end, and before `return`, `break`, `continue` and `?` leave it. If the type also has `def enter(self) -> T`, `name` is bound to its result; otherwise to the value itself. The plan named a `Scoped[T]` trait, but traits cannot take type parameters yet (D34); the methods give the same behaviour, and become a trait later without changing programs. A panic does not run `exit`: it ends the process (D24).

### D39. Modules: one scope per file, `pub` to export, and a loader in the driver

The command-line driver loads the main file, follows its imports depth-first (the main file's directory first, then the standard library), and reports cycles with the whole chain. The type checker then sees all modules at once, each with its own scope that starts with the prelude. `import a.b` binds `a.b` (or the `as` name); `from a.b import x` binds `x`. Bare enum variants are usable only where their enum is in scope. Functions in imported modules are named `module.function` in diagnostics and symbols. A `pub` declaration may still mention a private type in its signature; the planned check is left for later.

### D40. The standard library is written in the language and compiled into the compiler

`stdlib/io.lang` and `stdlib/os.lang` are embedded in the compiler with `include_str!`, like the prelude, so a program builds the same wherever it is. They reach the runtime through `@extern`. Because a C function cannot return a `Result`, a failing runtime call returns a placeholder and records a message per thread, which the module reads back to build its error. `str` crosses the C boundary as a pointer and a length (never NUL-terminated), and a returned `str?` is `None` when the pointer is null; binding C functions that expect NUL-terminated strings needs a small shim for now.

### D41. Sized numbers are for data and C, not for arithmetic

`i8` to `u64` and `f32` exist for binding C functions and compact data. They support conversion (`i32(x)`, `int(x)`, `float(x)`; integers wrap and floats truncate, saturating), comparison, hashing, printing, `+`, `-`, `*`, bit operations and, for `f32`, `/`. Floor division, modulo and powers need `int` or `float`; the error says to convert. Integer literals take a sized type from context and are checked against its range. `i64` and `f64` are other names for `int` and `float`.

### D42. Tasks are OS threads registered with Boehm, and shared objects live in collected memory

`spawn f(args)` evaluates the arguments in the spawning task, stores them in an environment object, and starts a thread (8 MiB of stack, reserved but committed on use) that registers itself with the collector, calls the function, and stores the result in the task object. Until the new thread has registered, a two-word uncollectable root keeps the environment and task alive. A `Task`'s result slot, a `Channel`'s ring buffer and a `Mutex`'s value are in collected memory, which the collector scans; their locks are Rust's futex-based `Mutex` and `Condvar`, built in place, which hold no other memory. Received slots are cleared so a channel does not keep values alive. At most 10,000 tasks run at once (`WEBY_MAX_TASKS`); `spawn` waits beyond that.

### D43. `with m.lock() as value:` gives the value, and writes it back

A mutex is only reachable through `with m.lock() as value:`; `m.lock()` anywhere else is an error, since nothing would release the lock. `value` is the protected value; if the block assigns it (`n += 1` on a `Mutex[int]`), the new value is stored back before unlocking, on every way out of the block. `Mutex(x)` where `x` is a variable holding a mutable value is an error, since `x` would still reach the value without the lock.

### D44. What `Shareable` allows

As specified in docs/04, with these choices: an enum is shareable when all its variants' fields are; a trait object when its trait requires `Shareable`; a type parameter when it is bounded by `Shareable`; a function value only when it is a named function, a method bound to a shareable value, or a lambda or nested function whose captured variables are all shareable, judged where it is written, since a function type says nothing about captures. A guarded value may be read, changed and passed as an argument inside its block; storing it in a variable, field or collection, returning it, sending it, spawning with it, capturing it, or using it after the block are errors (E0339), unless the part taken out is shareable.

### D45. ThreadSanitizer is deferred

The roadmap asks for the runtime to be run under ThreadSanitizer. That needs a nightly Rust toolchain (`-Zsanitizer=thread`), which this machine does not have; installing one is a download left to the user. The concurrency programs instead run a thousand times in release mode and a hundred times with the collector running before every allocation (`tests/stress.sh`). The command to run when a nightly toolchain is present: `RUSTFLAGS=-Zsanitizer=thread cargo +nightly build -p lang_rt --target x86_64-unknown-linux-gnu`, then link a test program against that `liblang_rt.a` with `WEBY_RUNTIME`.

### D46. The collector's multi-threaded mode starts with the first `spawn`

Boehm needs `GC_allow_register_threads` before any other thread registers, and once called, every allocation takes its thread-safe path, about 50% slower on allocation-heavy code. It is called on the first `spawn`, which is always on the main thread. Single-threaded programs never pay for threads; a program that spawns pays from then on.

### D47. JSON: generated code, a strict scanner, and a hand-written corpus

`json` is a standard library module written in the language. Its `Value` is an ordinary enum; `encode`, `decode` and `parse` are thin wrappers over two compiler intrinsics. Code generation writes an encoder and a decoder per type: keys are matched in generated code, values go straight into fields, and there is no intermediate tree. The scanner follows RFC 8259 strictly (no comments, trailing commas, `NaN`, leading zeros or lone surrogates; at most 512 levels of nesting). Encoding is compact, like Go's; floats are written as Python writes them (`1.0`, `1e+100`), and NaN and the infinities as `null`. Integers that do not fit in `int` decode as floats in a `json.Value` and are an error for an `int` field. The conformance corpus in `tests/json/` was written for this project in the style of JSONTestSuite, since fetching the suite is a download not yet approved; running the real suite is a small follow-up.

### D48. Indexing user types, and literals as `json.Value`

`x[i]` on a struct or enum calls `get_index(self, i: int)` when the index is an `int`, and `get_item(self, key)` otherwise, if the type has them. That is how `data["user"]["name"]` and `data["items"][0]` work on `json.Value`, and it is open to any type. Where a `json.Value` is expected, `True`, numbers, strings, `None`, and list and dict literals become values, so `json.object({"ok": True, "tags": ["a"]})` needs no wrapping.

### D49. The HTTP parser is hand-written, and the router is written in Weby

docs/05 planned `httparse` for parsing and `matchit` for routing. Both are crates not yet on this machine, and adding them is a download, so the runtime has its own request-line, header and chunked-body parser (about 400 lines, with the limits and timeouts listed in docs/05), and the router lives in `stdlib/http.lang`: a list of routes tried in order, with `{name}` and trailing `{rest...}` segments. A path that matches a route under another method gets 405 with an `Allow` header. Trying routes in order is linear in the number of routes; a radix tree is an optimisation for when services have hundreds of routes. Writing the router in Weby now is where D10 planned to end up anyway.

### D50. Stage 1 HTTP: a thread per connection, and a callback per request

`lrt_http_serve` runs the accept loop on the calling thread and serves each connection on a thread registered with the collector, counted against the task cap. For each request it calls a compiled closure with an integer handle to the parsed request, which lives on the connection thread's stack and holds no collected objects; the closure reads what it needs through `lrt_http_req_*` (method, path, query, a header, the body) and leaves its response with `lrt_http_respond`. Only what the handler asks for becomes a Weby string. SIGINT and SIGTERM stop the accept loop, and `serve` returns once requests in progress have finished.

### D51. `http.Error` is a function, and errors carry a status

`http.Error(404, "...")` is a function returning an `HttpError` struct, because the prelude's `Error` trait owns the name `Error` in type position; a standard library module may define a value with the same name as a prelude type. The prelude's `Error` trait gained a default method `status(self) -> int` returning 500, which `HttpError` overrides and `json.JsonError` overrides with 400. The error mapping in `Server.handle` asks any error for its status, so a user's own error type can choose its response code by implementing `status`.

### D52. Route adapters are generated as source code and checked like user code

`server.mount(value)` generates, for each annotated method of the value's type, a nested function in the call style (shown in `check/http.rs`) plus the `server.route(...)` call, as source text. That text is parsed and type-checked in the scope of the `mount` call, with the types it names bound to temporary aliases so it works whatever the user imported. There is no special code generation for routes, and the generated code is exactly what docs/05 says annotations are sugar for. Generated files are exempt from the check that rejects registering an unshareable handler, since the mounted value itself is checked. A query parameter's default must be a literal, because it is copied into the generated code.

### D53. Type aliases

`type Handler = def(Request) -> Result[Response]` names a type; an alias can be `pub`, and types may be parenthesised, so `(def(int) -> str)?` is an optional function. Aliases cannot have type parameters.

### D54. A local variable may shadow a function or constant

A variable such as `sum` or `text` in a function body may now have the name of a built-in or a module-level function or constant; inside that function the name means the variable from its first assignment on. Before, it was an error, which made common names unusable once a module defined a function with that name (the `http` module defines `text` and `json`). A variable may still not shadow a type or a module.

### D55. Logging

`log.debug`, `info`, `warn` and `error` write one line to standard error with a timestamp and level. The level comes from `WEBY_LOG` (default `info`) or `log.set_level`. The runtime formats the whole line first and writes it while holding standard error's lock, so lines from concurrent tasks do not interleave.

### D56. The HTTP Python baseline is the standard library server

docs/07 compares against a Python framework. None is installed, and installing one is a download, so the baseline is `http.server.ThreadingHTTPServer` with Nagle's algorithm turned off. A framework such as FastAPI under uvicorn would be faster than this baseline but still well behind Weby; the comparison should be rerun with one when downloads are approved.

### D57. `bytes` is a prelude struct over a string's representation

WebSocket binary messages need a type for data that is not text. `bytes` is a `@frozen` struct in the prelude holding a private (pointer, length) string whose contents need not be UTF-8; only the prelude's own methods, backed by runtime functions that never treat the data as text, look inside it. That made it a few dozen lines instead of a new built-in type through the checker and code generator: `text.encode()`, `len`, indexing (through `get_index`, D48), `slice`, `+` (through `AddNumber`), `==`, hashing, `decode() -> str?`, `hex()` and `b'...'` printing. `len(x)` now calls a `len(self) -> int` method on any struct or enum that has one. Indexing gives an `int`, not a `u8`, since sized numbers are for data layout and C (D41). Slicing syntax (`b[1:3]`) and `bytearray` are not there; a `bytes` built-in type can replace the struct later without changing programs.

### D58. WebSockets run on the connection's task; a `WebSocket` is shareable

A WebSocket endpoint is a `GET` route whose handler calls `http.upgrade`: the runtime checks the handshake, writes the 101 response, hands the connection's stream (and any bytes already read past the request) to a socket registered under an id, and the endpoint's handler then runs on the connection's own task until it returns. `WebSocket` is a `@frozen` struct of the id, the path and the path parameters, so it can be stored in a `Mutex[dict[int, http.WebSocket]]` and sent to from any task; the runtime serialises writes with a lock per socket. Reads happen on the handler's task. The upgrade request's query and headers are copied into the socket and read through `ws.query` and `ws.header`, because `http.Request` is mutable and would make `WebSocket` unshareable. A handler that returns makes the runtime send a close frame (1000), wait up to two seconds for the peer's, and close the connection; at shutdown, open sockets are closed with 1001. Close codes accepted from a peer are 1000 to 1003, 1007 to 1011 and 3000 to 4999, as Autobahn expects.

### D59. WebSocket conformance is checked by a hand-written client

The roadmap's bar is Autobahn's fuzzing client with no failures in the non-extension cases. Fetching it is a download, so `crates/lang_cli/tests/websocket.rs` sends the same kinds of frames by hand: payload lengths at the header boundaries, pings and pongs, reserved bits and opcodes, fragmentation with interleaved control frames, invalid UTF-8 in messages and close reasons, valid and invalid close codes, and messages over the size limit, each checked for the echoed data or the close code Autobahn expects. It does not cover Autobahn's timing-dependent cases (frames split into one-byte writes, slow peers) beyond what the buffering already handles, nor the performance cases. Running the real suite remains a follow-up once downloads are approved.

### D60. `@http.ws` handlers return nothing

An `@http.ws` handler takes one `http.WebSocket` and may also take the upgrade `http.Request`, path parameters and query parameters, bound by the same rules as other routes; a body parameter or `status=` is an error. It must return nothing: there is no response to make once the connection is upgraded, and errors on the connection show up as `recv` returning `None`.

### D61. Fibers are hand-written: the context switch, the stacks and the scheduler

This replaces D42 and D50. `corosensei` and `crossbeam` would have been the obvious crates, and adding them is a download, so the runtime has its own (`crates/lang_rt/src/fiber.rs` and `poll.rs`, about 1,250 lines with comments). The context switch is about twenty instructions of x86-64 assembly that save the callee-saved registers and the MXCSR and x87 control words. Each stack is an `mmap` reservation, 1 MiB by default (`WEBY_TASK_STACK`), with a guard page; only touched pages use memory. Up to 16,384 stacks are kept for reuse, which takes task creation from 3.4x to 2.7x Go's time, at the cost of keeping their touched pages. Each worker has a FIFO run queue behind a lock; an idle worker takes from the global queue, then steals half of another's, then waits in `epoll_wait`. Parking uses a permit and a state word, so that a wake-up racing with going to sleep is never lost. There is one worker per core (`WEBY_WORKERS`), and no cap on tasks: the 10,000-task cap existed to protect against thread exhaustion.

### D62. How fibers and the Boehm collector cooperate

Boehm scans each registered thread from its stack pointer to the stack bottom it was told about. A worker switching to a fiber calls `GC_set_stackbottom` with the fiber's stack top, and back to its own when the fiber switches out, both inside `GC_alloc_lock`, which the collector holds while it stops the world and marks, so no collection sees a stack pointer on one stack and a bottom on another. Fibers that are not running are reported through `GC_set_push_other_roots` (chaining Boehm's own hook, which pushes the thread stacks): the live part of each stack, and a small array of each fiber's references (its entry's environment and task, and the mutexes it holds). The list of fibers changes only under the same lock. This is the cost of the design: two lock acquisitions per switch. At 690,000 requests per second on 12 workers it has not shown up as a bottleneck. (Replaced in D84: the switch takes no lock, and the collector works out each worker's stack from its stack pointer.)

### D63. The main function stays on the main thread

`main` runs on the main thread as before; only spawned tasks and HTTP connections are fibers. When the main thread waits (a `join`, a channel), it blocks the thread instead of parking. The workers start on the first `spawn` or `serve`, so a program that does neither never starts a thread and keeps D46's single-threaded allocation speed. Waiting code calls one `park` that does the right thing in either context.

### D64. A panic in a task unwinds to the task's entry

docs/04 planned to discard a panicking fiber's stack and run a list of cleanups recorded by `with`. Instead, a panic in a fiber unwinds with Rust's unwinder to `catch_unwind` at the fiber's entry, so Rust frames in between (runtime functions that call back into compiled code) are cleaned up properly. That needs unwind tables in generated code, so functions are now marked `uwtable` instead of `nounwind`, and runtime entry points use the `C-unwind` ABI. Generated code has no landing pads, so a `with` block that a panic leaves does not run its `exit`; the runtime instead tracks the mutexes each fiber holds and releases them when it panics, which is the cleanup that matters for other tasks. The value in such a mutex is as it was before the block, since a guarded value is written back only when the block ends normally (D43). Joining a panicked task panics in the joiner. A panicking HTTP handler gets a 500 response and its connection is closed. A panic on the main thread still ends the process with status 101 (D24).

### D65. Sockets: edge-triggered epoll, shared by the workers, with deadlines swept

A connection's socket is non-blocking and registered once with one epoll instance (`EPOLLIN | EPOLLOUT | EPOLLRDHUP | EPOLLET`). A read or write that would block records the fiber as the waiter for that direction and parks; any worker that calls `epoll_wait` marks the direction ready and wakes the waiter. Busy workers poll without waiting every 61 scheduling rounds, so the network is not starved when no worker is idle. The registration records are never freed, only recycled, so an event that arrives after a socket is closed is a harmless spurious wake-up. Read deadlines (the idle and request timeouts, WebSocket keep-alives) are not put in a timer heap, since almost every one is cancelled by data arriving first; the timer thread instead checks waiting sockets' deadlines every 250 ms. Explicit timers (`sleep`, `join_timeout`) go in a heap served by the same thread. At shutdown, every waiting socket is woken so its fiber sees the flag. An idle connection costs about 11 KiB: its stack's touched pages, a 4 KiB read buffer that grows only when needed, and the runtime's records.

### D66. What stage 2 leaves out

- **A blocking pool.** File I/O and `@extern` calls run on the worker that makes them, which stalls the fibers queued there for that long. Reading standard input in a task can therefore hold a worker indefinitely.
- **Preemption.** A fiber runs until it waits. A CPU-bound loop holds its worker; with one worker per core, as many such loops as cores stop everything else. The planned fix (a check on loop back-edges) is on the optimisation backlog.
- **Clean stack overflow.** Overflowing a fiber's stack hits its guard page and kills the process with a segmentation fault. Turning that into a panic needs a signal handler on an alternate stack.
- **Per-worker timers and aarch64.** One timer thread serves all workers; the context switch exists only for x86-64.

### D67. Debug information is line tables, frame pointers are always on, and panics print a backtrace

Every compiled function gets a DWARF subprogram and every expression a source location; locals and types are not described yet, because describing the language's value layouts (strings, optionals, results, closures) to a debugger is its own project. That is enough for breakpoints by line, stepping, `perf report` with source lines, and backtraces. Functions keep frame pointers in release builds too, as Go's do, so `perf record -g` works without DWARF unwinding; it costs about 10% on `fib`, which is nothing but calls, and nothing measurable elsewhere. A panic prints the chain of calls in the program's own source (`Point.scaled at app.wb:6:22`), symbolised by the Rust standard library's backtrace support from the same DWARF; `WEBY_BACKTRACE=0` turns it off.

### D68. `fmt` keeps line breaks and comments, and checks itself with the lexer

The formatter scans the text itself, since the lexer drops comments, and normalises spacing, indentation and blank lines without moving code between lines: what is on one line stays on one line, and a multi-line call stays multi-line with its continuation lines indented one level. Choosing line breaks (fitting to a width) is the larger half of a formatter like Black's and was left out. The output must lex to exactly the tokens of the input, or the file is left alone and the problem reported; the repository's own test files pass that check and are stable under reformatting. The rules match the style the repository was already written in, so it changed only three files, all in aligned trailing comments.

### D69. Tests run as tasks, and `assert` is a statement

A test is a `@test` function; `weby test` appends a generated runner to the file's source (so source positions stay correct) that spawns each test as a task and asks whether it panicked, through a task method meant for the runner. This is milestone 10's task-scoped panics put to use: a failed test reports its panic and the others still run. `assert cond, message` became a statement, parsed into `if not cond: panic(message)`; `assert` is now a keyword. Tests in imported modules are not collected: tests live with the code in the file being tested.

### D70. The language server re-checks the whole program on each change

`weby lsp` reads full-document updates, runs the same front end as `check` with an index switched on (each expression's type, and where each variable, function, method, field and type it names is defined), and answers hover and go-to-definition from that index. Each change re-checks everything, which takes milliseconds for programs of the sizes in this repository; incremental checking waits until programs are large enough to need it. A missing `main` is not reported, since the file may be a module. The JSON-RPC layer and JSON parser are written by hand (about 250 lines), since adding `serde` is a download.

### D71. Dependencies: a small manifest, git or path sources, and a lock file

`weby.toml` names dependencies as `{ git = "url", rev = "tag or branch or commit" }` or `{ path = "dir" }`. A git dependency is cloned into `.weby/deps/<name>` inside the project when first needed, and the commit used is written to `weby.lock`; later builds check out that commit, so a build does not change because a branch moved. `weby fetch --update` moves each dependency to its `rev` and rewrites the lock file. `import dep.module` loads from the dependency; `import dep` loads its `lib` file; a dependency's own files import each other with its name first, like Go's import paths. There is no version solving, no registry, and no handling of two dependencies wanting different versions of a third; those wait for a real package ecosystem. The manifest format is a subset of TOML, parsed by hand.

### D72. Editor support is a grammar plus the language server

A TextMate grammar (`editors/vscode/`) covers VS Code and the many editors that read that format, and a Vim syntax file covers Vim and Neovim; both are maintained by hand against the keyword list. Neovim and Helix can start `weby lsp` with a few lines of configuration (`editors/README.md`). A VS Code extension that starts the language server needs `vscode-languageclient` from npm, a download, and is not included.

### D73. The collector grows the heap to about twice the live data

Boehm's default free-space divisor of 3 starts a collection when about a third of the heap has been allocated since the last one, which is far more often than Go's default (`GOGC=100`: collect when the heap has doubled). Setting the divisor to 1 matches Go's policy. It made the allocation-heavy HTTP endpoint 54% faster (with the response change below) and batch benchmarks 3% to 7% faster, for 10% to 25% more memory (`binary_trees`: 72 MB against 57 MB). `WEBY_GC_FREE_SPACE_DIVISOR` still overrides it; a memory-tight deployment can set 2 or 3. (The default became 2 in D86, once D84's allocation floor had taken over small heaps.)

### D74. The compiler recognises single-lookup dict updates

`d[k] = d.get_or(k, default) OP x` and `d[k] = d[k] OP x`, where `d` and `k` are variables and `x` and `default` are variables or constants, compile to one lookup: the runtime's find-or-insert returns the value's slot, the dict's length says whether it was inserted, and the new value is stored into the slot. The conditions guarantee nothing between the lookup and the store can change the dict. This is the idiom programs already use, so they get faster unchanged; an `entry` method would have needed closures or a mutable reference type the language does not have. `word_count` went from 1.25 s to 0.78 s.

### D75. Responses are serialised in the call that gives them

`lrt_http_respond` used to copy the body into a buffer, which the connection then copied again into the bytes it wrote. It now writes the status line, headers and body into the final buffer directly, while the handler's strings are still reachable. Whether the connection stays open is decided before the handler runs; a server shutting down in between still closes the connection after the response.

### D76. Regular expressions use `regex-lite`

`re` is backed by the `regex-lite` crate, which was already in the local Cargo cache (no download), has no dependencies, and guarantees linear-time matching, so a pattern from a user cannot make a server spin. It has no Unicode classes beyond what its syntax lists and is slower than the full `regex` crate on large texts; switching later changes no program. Compiled patterns are kept for the life of the program, which suits compiling them once at startup; a program compiling patterns in a loop grows. Patterns are ordinary strings, since the language has no raw strings yet.

### D77. The HTTP client is plain HTTP/1.1, one request per connection

`http.fetch` connects, sends `Connection: close`, and reads the whole response (sized, chunked, or to the end of the stream), with a 30-second deadline, over the same non-blocking socket as the server, so a waiting task parks. Name resolution uses the system resolver, which blocks the worker for its duration (there is no blocking pool, D66). There is no connection reuse, no redirects and no streaming. TLS came later (D79). A `ClientError` returned from a handler maps to 502. Connecting was at first a blocking `connect` with a timeout, which held a worker for as long as an unreachable host took to fail; since D79 the connect itself is non-blocking and the task parks.

### D78. Headers and cookies bind by type

A handler parameter of type `http.Header` is the header named after the parameter (`user_agent` is `User-Agent`), and `http.Cookie` the cookie of that name; `T?` makes either optional, and otherwise a missing one is a 400 naming it. Binding by type keeps the rule table fixed and readable (R11): the parameter's type says where the value comes from, as it does for bodies.

### D79. TLS is `rustls` inside the socket

TLS uses `rustls` 0.23 with the `ring` provider (`aws-lc-rs`, the default, needs CMake and a longer build), `webpki-roots` for the Mozilla root store, and nothing from the system: a program does not depend on the machine's OpenSSL or certificate directory, which keeps binaries portable and the trust decision reviewable. `SSL_CERT_FILE` adds roots, the convention OpenSSL and Go share, for private authorities and tests.

The session lives inside the runtime's `Socket`, beside the non-blocking descriptor, so every protocol on top (HTTP server and client, WebSockets, PostgreSQL) gets TLS without knowing. Reads and writes drive `rustls` by hand: plaintext it already has first, then any records it wants written, then more records from the socket, parking the task on `WouldBlock`. The session is behind a short-held mutex, never held across a park, because a WebSocket reads in one task while another writes. The server handshake runs on the connection's first read, in its own task, so a slow or hostile handshake ties up nothing else. ALPN is offered per protocol (`http/1.1`; `postgresql`, which PostgreSQL 17 insists on if any is offered).

Measured: 1 MiB bodies each way and WebSocket messages larger than a TLS record pass through intact, and a plain-HTTP client on the TLS port is turned away without stopping the server (`crates/lang_cli/tests/tls.rs`).

### D80. The PostgreSQL driver is runtime code with text parameters

The wire protocol (version 3) is implemented in the runtime, about 1,200 lines with its unit tests, rather than wrapping libpq or a Rust crate: libpq blocks the calling thread, the Rust drivers are built on async runtimes this one is not, and the protocol is small. SCRAM-SHA-256 uses `ring`'s PBKDF2, HMAC and SHA-256, already present for TLS; MD5 for old servers is hand-written.

Every query uses the extended protocol with all parameters and results in text format. Parameters are `list[str?]` because the language cannot put an `int` and a `str` in one list without a wrapper type (only structs coerce to trait objects), and a wrapper enum makes every call site noisy; the server types text parameters from the statement, so nothing is lost but the `str(n)` at the call. Results come back as text and are converted on request (`row.int`) or, for `query_as[T]`, turned into JSON by the runtime (numbers, booleans and `json` columns as themselves) and decoded by the `FromJson` code `@derive(Json)` already generates. That reuses the most heavily tested decoder in the system instead of adding a `@derive(Row)`.

The pool is per `connect` call and lives in the runtime; a `Db` is a frozen struct holding its number, so it is shareable by construction. A transaction is a `Db` that holds one connection; `with` rolls it back unless `commit` ran. A connection is returned to the pool only if its last ReadyForQuery said idle; one whose socket failed is closed. The SQLSTATE of each failure is kept, so a program can tell a unique violation (`23505`) from a lost connection (`08006`).

Not done: statement caching (each query is parsed each time; a cache keyed by SQL text is the next step for speed), `COPY`, `LISTEN`/`NOTIFY`, cancellation, and retrying a query whose pooled connection the server had already closed.

### D81. Conformance suites run from scratch directories, not from CI

JSONTestSuite and the Autobahn WebSocket suite are run by hand (`tests/conformance/README.md`), against programs that live in the repository. Autobahn's suite only runs on Python 2; it was installed into a portable PyPy 2.7 with its TLS dependency left out (the fuzzing client does not use it). Neither suite is vendored, and CI does not run them, because each needs a download and Autobahn takes minutes. Results are recorded in [07](07-testing-and-performance.md#protocol-conformance).

### D82. The compiler tells LLVM what the runtime does (performance plan, phase 1)

LLVM could not optimise around runtime calls, because it knew nothing about them ([09](09-performance-plan.md)). Phase 1 gives it that information instead of changing any layout:

- **Allocation.** Structs, enum variants, captured-variable cells and closure environments are allocated by `lrt_alloc_raw`, which is declared as an allocator:
  - `allockind("alloc,zeroed")`, `allocsize(0)` and an allocation family;
  - a `noalias`, 16-byte aligned result;
  - `memory(inaccessiblemem: readwrite)`, `nounwind`, `willreturn`.

  Generated code now stores the descriptor word itself, so the "zeroed" claim holds. LLVM removes allocations whose objects never escape once inlining exposes them: a loop building 50 million temporary structs went from 0.345 s to 0.001 s. It also drops the zeroing that used to follow every allocation.
- **Side effects of runtime functions.** A few string functions declare their memory effects: comparison, `find`, `starts_with` and `ends_with` read only their arguments, and `concat` also allocates. Only functions that cannot panic are marked `nounwind`.
- **Alias metadata (TBAA)** separates a list's length, capacity and data pointer, list elements, and each struct field. Loops keep the length and data pointer in registers across element and field stores. Accesses without a tag (runtime calls, `memcpy`, helpers) still alias everything, so a missed tag costs speed, never correctness.
- **`append`** is inline when the buffer has room: a compare, a store and a length update.
- **String `==`** is inline: lengths, then `memcmp`, which LLVM expands for constant strings.
- **String builders** for f-strings, `str(x)` and `print` are stack slots, not collected objects. Each f-string reserves its estimated length once. Integers are formatted without a Rust heap allocation.

**Rejected.** `-O3` and `-mcpu=native`: measured, no gain. Writing these fast paths in the runtime: the runtime is built by `rustc`'s LLVM 23, which LLVM 19 cannot read, so nothing in it can be inlined into generated code.

**Found on the way.** The JSON reader is Rust heap memory holding a pointer to the text it reads, which the "Rust heap rule" in `lang_rt` forbids. Generated code had kept the text alive by chance; once string comparison was inlined, a release build under collector stress freed the text mid-parse. `lrt_json_reader_free` now takes the text as an argument, which keeps it alive until then. No other runtime object holds a collected pointer from the Rust heap. Programs now also run as release builds under collector stress in `tests/run`, since that combination is what exposed it.

**A caveat on memory figures.** With a conservative collector, peak memory can move by a fifth on unrelated changes. One choice of f-string size estimate left a stale word that looked like a pointer to a tree, and `binary_trees` peaked at 90 MB instead of 73 MB. Every other estimate tried gave 73 MB. Compare memory across several builds before attributing a change to it.

### D83. Allocate less: inline allocation, pointer-free structs, escape analysis (performance plan, phase 2)

Phase 2 of [09](09-performance-plan.md) attacks the cost of each allocation and the number of allocations, without changing object layouts.

- **Inline allocation fast path** (`lang_codegen/src/alloc.rs`, `lrt_alloc_slow` in `lang_rt/src/gc.rs`).
  - **Each thread's free lists.** Each thread has an `AllocCache`: a free list of zeroed objects for each size up to 16 granules (255 bytes), refilled with `GC_malloc_many`, as Boehm's `gc_inline.h` describes.
  - **Lowered after optimisation.** Code generation still emits `lrt_alloc_raw(size)`, so LLVM can delete allocations that do not escape, as in D82. Only after the optimisation pipeline is each remaining constant-size call redirected to an always-inlined function that pops the thread's list: two loads, a compare and two stores, with no call. An empty list or a missing cache calls `lrt_alloc_slow`.
  - **Thread-local access.** The cache pointer is a thread-local defined in the program's object and read with side-effecting inline assembly (`movq %fs:lang_alloc_cache@tpoff`). LLVM assumes a thread-local's address is constant within a function, but a task can resume on another worker thread after any wait. The assembly can never be merged with an earlier read or hoisted out of a loop, so a task always pops from the list of the thread it is running on.
  - **Keeping free objects alive.** The cache is allocated with `GC_malloc_uncollectable`, so it is a root: the collector keeps every object on a list reachable through the heads and the link words, and never reclaims one while it waits.
  - **Stress mode.** With `WEBY_GC_STRESS`, the slow path never fills a cache, so every allocation still collects first.
- **Pointer-free structs** (numbers and bools only, in structs and enum variants) are allocated with `GC_malloc_atomic`, so the collector never scans them. Their memory is not zeroed, and `lrt_alloc_atomic_raw` says so to LLVM (`allockind("alloc,uninitialized")`). Every construction stores every field. They take the runtime call, not the inline path: a free list of objects the collector does not scan would not keep its own tail alive.
- **Escape analysis** (`lang_codegen/src/escape.rs`).
  - **What it puts on the stack.** A struct construction whose object provably never outlives the call is an `alloca` in the entry block. The analysis is flow-insensitive: each expression has a set of sources, which are construction sites and parameters.
  - **What counts as escaping.** A source escapes into returns, other objects, collections, closures, trait objects, tasks, f-strings, operators and `match`, and into any call except to a parameter that is itself known not to escape.
  - **Parameter summaries** cover the whole program and are iterated to a fixed point.
  - **Loops.** Each site has one slot per call, so a site inside a loop also needs at most one local that can hold its object: when the site runs again, that local is overwritten, so no reference to the previous object remains. `tests/run/escape.wb` covers the cases that must stay on the heap.
  - **What it adds.** After D82, LLVM already deletes allocations it can see through after inlining. This handles the ones it cannot, such as a temporary passed to a function that is not inlined: 0.071 s to 0.018 s for 5 million such calls.
- **The object header** stays, as the plan recommended: the decision waits for phase 5, since a precise collector is what the header was kept for.

**Results** ([bench/README.md](../bench/README.md)):
- `binary_trees` went from 0.97 s to 0.65 s, now faster than Go (0.83 s).
- `json_orders` is about 5% faster; the rest is unchanged.
- HTTP `/items` at 256 connections went from 0.91x to 0.93x Go's rate, short of the phase's goal of parity. Its time is not mostly in allocation, so the next step is phase 3's first item, the collector's lock on every context switch.

**Memory.** `binary_trees` peaks at 90 MB instead of 73 MB.
- The size classes are unchanged: `GC_malloc_many` gives 32-byte objects for the 24-byte node, as `GC_malloc` did.
- The live data at the end is the same, about 17 MB.
- At one mid-run collection, 25 MB was still reachable instead of 20 MB, and the heap kept the size it grew to then. This is the conservative-scanning effect described under D82: a word on the stack or in a register briefly looked like a pointer to a dead tree.

`json_orders` went from 81 to 88 MB.

**Rejected.**
- **Using LLVM's `HeapToStack`** (part of the Attributor) instead of a custom analysis. It is not in the standard pipeline, places its stack memory where the allocation was, which grows the stack on every loop iteration, and cannot see the program's own rules about what escapes.
- **Reading the thread-local through `llvm.threadlocal.address`.** It is the documented way, but nothing stops later passes from reusing the address across a task switch.

### D84. Runtime: the context switch, task creation, routing, and how often to collect (performance plan, phase 3)

Phase 3 of [09](09-performance-plan.md). The evidence comes from a small sampling profiler, `bench/profile/`, built because `perf` is not installed: a SIGPROF handler loaded with `LD_PRELOAD` that records instruction pointers and frame-pointer callers, symbolised with `llvm-symbolizer`.

- **No collector lock on the context switch** (`lang_rt/src/fiber.rs`, "The collector").
  - **How workers are scanned now.** Workers no longer move Boehm's "stack bottom" between stacks under the allocation lock. Boehm's sp corrector hands over each stopped worker's stack pointer, and the runtime's root hook decides which stack it is on from where it points: inside the stack of the fiber the worker says it is running, or else the worker's own stack.
  - **Why no stop can be missed.** The worker scans the live part of that stack and of the other one, from the pointer saved there. The switch saves one stack pointer before loading the other, so a worker stopped at any instruction is covered.
  - **Effect.** About 2% on `/plaintext`, nothing measurable on `/items`. Finding 7's guess that this lock limited small responses was wrong at this core count.
- **Cheaper `spawn`.** Profiling showed the main thread spending most of its time in contended locks (the global run queue, the stack pool, the fiber registry), in glibc's allocator locks (an `Arc` and a boxed closure per task, freed on another thread), and in futex calls. Now:
  - the fiber's record sits at the top of its own stack, with a reference count, so a task allocates nothing in Rust;
  - a task's entry is a function pointer, not a boxed closure;
  - each thread keeps up to 64 stacks and trades them with the shared pool 32 at a time;
  - a table of stack slots, set and cleared with single stores, replaces the locked registry of fibers;
  - the global run queue is an intrusive lock-free multi-producer queue (Vyukov's), with one worker taking at a time and the producer and taker fields on separate cache lines;
  - workers take a share of the global queue at once, and a worker is woken when the queue stops being empty, not for every task;
  - empty queues are seen without taking their locks.

  `spawn` went from 0.130 s to 0.044 s (Go: 0.042 s).
- **A router that does not allocate per route tried** (`stdlib/http.lang`).
  - **Precomputed segments.** Each route's segments are classified once.
  - **Fixed paths.** A path that is exactly a route's pattern without parameters (`/plaintext`, `/items`) is answered from a table built when the handler chain is composed. The table holds, in order, every route that path fits, with its parameters, so the answer is the one the full search would give, with no split and no dictionary.
  - **Other paths** are split once and matched without allocating. The parameters dictionary is built only for the route that matched, and requests to routes without parameters share one empty dictionary.
  - **Tests.** `tests/run/routing.wb` checks order, 404, 405 with `Allow`, `HEAD`, rest parameters, extra slashes, groups and late routes; it gives the same output with the old router.
- **Headers straight into the response.** `lrt_http_respond` takes the content type and writes the `Content-Type` line itself. The handler's other headers are joined into a string only when there are any.
- **A minimum allocation between collections** (`WEBY_GC_MIN_ALLOC`, default 4 MiB, Go's minimum heap).
  - **The finding.** `/items` was not limited by allocation, the router or the scheduler but by how often the collector ran. With little live data, Boehm (free-space divisor 1, D73) collects after about a megabyte of allocation: over 1,100 collections a second at 230,000 requests a second, each stopping every worker.
  - **The floor.** `GC_set_min_bytes_allocd` sets a floor on the bytes allocated between collections. With 4 MiB, collections fell by 70% and `/items` rose from 209,000 to 271,000 requests a second, for a peak of 24.5 MB against 17.5 MB, as much as Go uses. 2 MiB gave 260,000 at 20 MB; 8 MiB and more gave nothing further.

**Tried and dropped.**
- **Spinning idle workers** before they wait in the poller, as Go does.
  - With a locked global queue, every spinning worker took a core from the main thread, which is not a worker, and `spawn` got slower (0.20 s).
  - Limited to one spinner with the lock-free queue, it changed nothing.
- **Offsetting each fiber record by a per-stack amount** to spread them over cache sets made HTTP slightly slower.

**Results** (A/B runs of the same programs, 256 connections; the full tables are in [bench/README.md](../bench/README.md)): `spawn` 0.130 s to 0.044 s; `/items` 232,000 to 271,000 requests a second, now ahead of Go; `/users/42` 570,000 to 855,000; `/plaintext` and `/json` about 13% faster.

### D85. Code generation for dispatch and collections (performance plan, phase 4)

Phase 4 of [09](09-performance-plan.md).

- **Devirtualisation** (`lang_codegen/src/traits.rs`).
  - **Dispatchers.** Every call through a trait object goes through a private, always-inlined function for its trait method, `dispatch.Trait.method(vtable, data, args...)`. The function is declared when the first such call is generated, and its body is written after the whole program has been generated, when every type converted to that trait is known (vtables are made only by conversions, which are all in the program).
  - **The body.** With at most four types, it compares the vtable with each one's and calls that type's method directly; otherwise, or if nothing matches, it makes the indirect call through the vtable as before. A vtable made after a body was written is still handled by that last case.
  - **Effect.** After inlining, LLVM sees ordinary direct calls: in `dispatch` the three `area` methods are inlined, and the accumulator stays in a register. 0.052 s to 0.008 s, against Go's 0.030 s.
- **`str` keys in dicts** (`lang_rt/src/dict.rs`). A dict whose descriptor's hash function is the runtime's own string hash now hashes and compares keys directly: no indirect call per probe, and equal pointers short-cut the byte comparison.
- **Split and lower-case** (`lang_rt/src/strings.rs`).
  - `split` collects its parts first (on the stack for up to 32) and allocates the list once at its size.
  - Lower- and upper-casing check ASCII text byte by byte before falling back to Unicode.
  - Together with the dict change, `word_count` went from 0.73 s to 0.64 s.
- **Concatenation chains** (`lang_codegen/src/expr.rs`). `a + b + c + ...` on strings, three or more parts, evaluates the parts left to right and joins them with one call, `lrt_str_concat_n`, which allocates once at the full length. Empty parts are skipped, and a chain with one non-empty part shares it, as two-part `+` does. `tests/run/concat.wb`.
- **Not done.**
  - **Hand-written C helpers compiled to LLVM bitcode and inlined into programs (item 2).** This would add a build step that needs `clang-19`.
  - **Code-generated dict probe loops (item 3).** The exit criteria were met without either, so they wait until a profile shows the runtime's dict or UTF-8 code on a hot path again.
- **Tests.** `tests/run/dispatch.wb` covers one, three and five implementing types (the last stays indirect), upcasts through a supertrait, a method without a result, and methods with arguments.

**Results** ([bench/README.md](../bench/README.md)):
- `dispatch` takes 0.009 s against Go's 0.031 s; it was 1.73x Go's time.
- `word_count` takes 0.648 s against Go's 0.778 s, from 1.07x to 1.20x faster than Go.
- Every benchmark and every HTTP endpoint is now at or ahead of Go.

### D86. Stay on Boehm; collect a little more often (performance plan, phase 5)

Phase 5 of [09](09-performance-plan.md) was to be decided by measuring what the collector still costs after phases 1 to 4. The rule was to take up a precise, generational collector if collection were still more than about 15% of `/items`, or memory about twice Go's.

**What the collector costs now.**
- **Time.** Under `/items` at 256 connections, `libgc` takes 20% of the CPU (`bench/profile/`). Almost all of that is allocation, not collection. Raising the allocation floor from 4 MiB to 1 GiB, which cut collections from about 1,460 to 10 in 5 s, made the endpoint no faster (223,000 against 230,000 requests a second, within noise). World-stopped marking added up to 0.7 s of CPU out of about 30 CPU-seconds, about 2%.
- **A long-running server.** Under a minute of sustained load on `/items` and on `POST /orders`, resident memory stayed at 24 MB from the first ten seconds to the last. Conservative scanning is not making it grow.
- **Server memory.** Peak memory under load is level with Go's (22 to 24 MB against 23 to 24 MB).
- **Batch programs** used about twice Go's memory: `binary_trees` 90 MB against 42 MB, `sort` 146 MB against 79 MB. The causes are in how objects are laid out, not in how they are collected:
  - **`binary_trees`.** A node is an 8-byte header and two pointers, 24 bytes. Boehm adds one byte so that a pointer just past an object keeps it alive, and rounds to 16-byte granules, so each node takes 32 bytes; Go's takes 16.
  - **`sort`.** A `list[Record]` holds pointers to 48-byte objects, where Go's slice holds 32-byte values inline. That is a question of value types in collections, which no collector changes.

**Decision: stay on Boehm.** Collection time is far under the threshold. Of the memory gap, a precise collector such as MMTk's Immix would remove the extra byte and the rounding: 24-byte nodes, 25% less for `binary_trees`. It would not remove the header (the collector needs type information somewhere) or the boxed list elements. That is not worth a new runtime dependency, stack maps through the code generator, and the risk of a moving collector. The precise-collector path in [04](04-runtime-and-memory.md#beyond-boehm) stays the plan for when pause times or heap size become the problem, for example a server holding gigabytes of live data. The object header stays too (phase 2, item 3): it is what that path would use.

**What changed: the free-space divisor default goes from 1 to 2.** D73 chose 1 to make small heaps collect less often, but D84's 4 MiB floor now does that job for servers and small programs. On larger heaps, 1 cost a lot of memory for a little time:

| Benchmark | Divisor 1 | Divisor 2 |
|---|---|---|
| `binary_trees` | 0.690 s, 90 MB | 0.725 s, 64 MB |
| `json_orders` | 0.583 s, 78 MB | 0.603 s, 71 MB |
| `spawn` | 0.045 s, 19 MB | 0.044 s, 13 MB |
| `/items`, 256 connections | 238,600 req/s, 23 MB | 235,100 req/s, 22 MB |

The other benchmarks are unchanged. `WEBY_GC_FREE_SPACE_DIVISOR=1` restores the old behaviour.

**Not done, and why.**
- **Asking Boehm for one byte less for objects whose size is a multiple of 16.** This would avoid the extra byte where it costs a whole granule, but a pointer just past such an object would then keep the next object alive instead of this one, and LLVM may keep such pointers. Not worth that risk.
- **Turning off interior pointers.** Substrings share their parent's bytes at any offset, so the collector must recognise pointers into the middle of objects.

**Results** (full runs, [bench/README.md](../bench/README.md); the machine was about 10% slower than for phase 4, as Go's own times show):
- `binary_trees` peaks at 65 MB against Go's 40 MB, 1.6x, down from 2.1x; it is still 1.27x faster than Go.
- `json_orders` 71 MB against 59 MB.
- Every benchmark is still at or ahead of Go. `/items` is level with Go (1.01x), and the other HTTP endpoints are 1.8x to 2.1x faster.

## Risks

| # | Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|---|
| R1 | Boehm cannot keep up with Go on allocation-heavy request handling | High | Medium | Use pointer-free allocation for strings and byte buffers; reduce allocation in the HTTP path; escape analysis; precise GC as the planned exit. Measured at the M8 gate. |
| R2 | A GC reference is held only in Rust-allocated memory and gets collected | Medium | High (memory corruption) | The Rust heap rule in [04](04-runtime-and-memory.md#the-rust-heap-rule); a single handle-table API; a GC stress mode (collect on every allocation) run in CI. |
| R3 | Fibers plus conservative stack scanning across worker threads is fragile | Medium | High | Own the fiber implementation; register stacks through the documented Boehm hook; stage 2 is after the prototype, so the prototype does not depend on it. As built (D62), every concurrency and server test passes with a collection before every allocation, and the stress script ran a thousand times without a failure. |
| R4 | Scope creep in the language (features before the web stack works) | High | High | The roadmap's exit criteria are programs, not features. Anything not needed by a milestone's demo waits. |
| R5 | Indentation lexing and f-strings produce confusing errors | Medium | Medium | Dedicated diagnostics tests from milestone 1; tabs are rejected outright. |
| R6 | Generic syntax `f[T](x)` is ambiguous with indexing | Certain | Low | Parse as an index expression; the type checker reinterprets it when the base names a generic. Documented in [03](03-compiler-architecture.md#parser). |
| R7 | Trait system grows into Rust's (associated types, blanket impls, specialisation) | Medium | Medium | Explicit feature fence in [02](02-type-system-and-traits.md#what-the-trait-system-deliberately-lacks). |
| R8 | A panic in one request handler takes down the server | Resolved in M10 | Medium | A panic now ends only its task, and a handler's becomes a 500 (D64). |
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
