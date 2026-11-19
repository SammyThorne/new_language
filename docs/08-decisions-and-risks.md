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
