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

**Constraint that must be respected.** Boehm does not scan memory allocated by Rust's allocator. Any reference to a Viper object held by the runtime must live in GC-visible memory. See [04](04-runtime-and-memory.md#the-rust-heap-rule).

**Rejected.** C runtime: closer to Boehm, worse for everything else. Writing the runtime in Viper: the long-term direction for the library layer, impossible at the start.

### D10. The web stack is implemented in the runtime first, then migrated into Viper

**Why.** HTTP, WebSocket framing and JSON scanning can be working within weeks in Rust. Writing them in Viper needs generics, byte buffers, raw I/O and a mature optimiser. The Viper-facing API is the stable contract; what sits behind it can move.

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

### D14. The language is called Viper, with `.vi` source files

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

### D33. Methods do not have their own type parameters yet (amended by D94: they can)

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

`spawn f(args)` evaluates the arguments in the spawning task, stores them in an environment object, and starts a thread (8 MiB of stack, reserved but committed on use) that registers itself with the collector, calls the function, and stores the result in the task object. Until the new thread has registered, a two-word uncollectable root keeps the environment and task alive. A `Task`'s result slot, a `Channel`'s ring buffer and a `Mutex`'s value are in collected memory, which the collector scans; their locks are Rust's futex-based `Mutex` and `Condvar`, built in place, which hold no other memory. Received slots are cleared so a channel does not keep values alive. At most 10,000 tasks run at once (`VIPER_MAX_TASKS`); `spawn` waits beyond that.

### D43. `with m.lock() as value:` gives the value, and writes it back

A mutex is only reachable through `with m.lock() as value:`; `m.lock()` anywhere else is an error, since nothing would release the lock. `value` is the protected value; if the block assigns it (`n += 1` on a `Mutex[int]`), the new value is stored back before unlocking, on every way out of the block. `Mutex(x)` where `x` is a variable holding a mutable value is an error, since `x` would still reach the value without the lock.

### D44. What `Shareable` allows

As specified in docs/04, with these choices: an enum is shareable when all its variants' fields are; a trait object when its trait requires `Shareable`; a type parameter when it is bounded by `Shareable`; a function value only when it is a named function, a method bound to a shareable value, or a lambda or nested function whose captured variables are all shareable, judged where it is written, since a function type says nothing about captures. A guarded value may be read, changed and passed as an argument inside its block; storing it in a variable, field or collection, returning it, sending it, spawning with it, capturing it, or using it after the block are errors (E0339), unless the part taken out is shareable.

### D45. ThreadSanitizer is deferred

The roadmap asks for the runtime to be run under ThreadSanitizer. That needs a nightly Rust toolchain (`-Zsanitizer=thread`), which this machine does not have; installing one is a download left to the user. The concurrency programs instead run a thousand times in release mode and a hundred times with the collector running before every allocation (`tests/stress.sh`). The command to run when a nightly toolchain is present: `RUSTFLAGS=-Zsanitizer=thread cargo +nightly build -p lang_rt --target x86_64-unknown-linux-gnu`, then link a test program against that `liblang_rt.a` with `VIPER_RUNTIME`.

### D46. The collector's multi-threaded mode starts with the first `spawn`

Boehm needs `GC_allow_register_threads` before any other thread registers, and once called, every allocation takes its thread-safe path, about 50% slower on allocation-heavy code. It is called on the first `spawn`, which is always on the main thread. Single-threaded programs never pay for threads; a program that spawns pays from then on.

### D47. JSON: generated code, a strict scanner, and a hand-written corpus

`json` is a standard library module written in the language. Its `Value` is an ordinary enum; `encode`, `decode` and `parse` are thin wrappers over two compiler intrinsics. Code generation writes an encoder and a decoder per type: keys are matched in generated code, values go straight into fields, and there is no intermediate tree. The scanner follows RFC 8259 strictly (no comments, trailing commas, `NaN`, leading zeros or lone surrogates; at most 512 levels of nesting). Encoding is compact, like Go's; floats are written as Python writes them (`1.0`, `1e+100`), and NaN and the infinities as `null`. Integers that do not fit in `int` decode as floats in a `json.Value` and are an error for an `int` field. The conformance corpus in `tests/json/` was written for this project in the style of JSONTestSuite, since fetching the suite is a download not yet approved; running the real suite is a small follow-up.

### D48. Indexing user types, and literals as `json.Value`

`x[i]` on a struct or enum calls `get_index(self, i: int)` when the index is an `int`, and `get_item(self, key)` otherwise, if the type has them. That is how `data["user"]["name"]` and `data["items"][0]` work on `json.Value`, and it is open to any type. Where a `json.Value` is expected, `True`, numbers, strings, `None`, and list and dict literals become values, so `json.object({"ok": True, "tags": ["a"]})` needs no wrapping.

### D49. The HTTP parser is hand-written, and the router is written in Viper

docs/05 planned `httparse` for parsing and `matchit` for routing. Both are crates not yet on this machine, and adding them is a download, so the runtime has its own request-line, header and chunked-body parser (about 400 lines, with the limits and timeouts listed in docs/05), and the router lives in `stdlib/http.lang`: a list of routes tried in order, with `{name}` and trailing `{rest...}` segments. A path that matches a route under another method gets 405 with an `Allow` header. Trying routes in order is linear in the number of routes; a radix tree is an optimisation for when services have hundreds of routes. Writing the router in Viper now is where D10 planned to end up anyway.

### D50. Stage 1 HTTP: a thread per connection, and a callback per request

`lrt_http_serve` runs the accept loop on the calling thread and serves each connection on a thread registered with the collector, counted against the task cap. For each request it calls a compiled closure with an integer handle to the parsed request, which lives on the connection thread's stack and holds no collected objects; the closure reads what it needs through `lrt_http_req_*` (method, path, query, a header, the body) and leaves its response with `lrt_http_respond`. Only what the handler asks for becomes a Viper string. SIGINT and SIGTERM stop the accept loop, and `serve` returns once requests in progress have finished.

### D51. `http.Error` is a function, and errors carry a status

`http.Error(404, "...")` is a function returning an `HttpError` struct, because the prelude's `Error` trait owns the name `Error` in type position; a standard library module may define a value with the same name as a prelude type. The prelude's `Error` trait gained a default method `status(self) -> int` returning 500, which `HttpError` overrides and `json.JsonError` overrides with 400. The error mapping in `Server.handle` asks any error for its status, so a user's own error type can choose its response code by implementing `status`.

### D52. Route adapters are generated as source code and checked like user code

`server.mount(value)` generates, for each annotated method of the value's type, a nested function in the call style (shown in `check/http.rs`) plus the `server.route(...)` call, as source text. That text is parsed and type-checked in the scope of the `mount` call, with the types it names bound to temporary aliases so it works whatever the user imported. There is no special code generation for routes, and the generated code is exactly what docs/05 says annotations are sugar for. Generated files are exempt from the check that rejects registering an unshareable handler, since the mounted value itself is checked. A query parameter's default must be a literal, because it is copied into the generated code.

### D53. Type aliases

`type Handler = def(Request) -> Result[Response]` names a type; an alias can be `pub`, and types may be parenthesised, so `(def(int) -> str)?` is an optional function. Aliases cannot have type parameters.

### D54. A local variable may shadow a function or constant

A variable such as `sum` or `text` in a function body may now have the name of a built-in or a module-level function or constant; inside that function the name means the variable from its first assignment on. Before, it was an error, which made common names unusable once a module defined a function with that name (the `http` module defines `text` and `json`). A variable may still not shadow a type or a module.

### D55. Logging

`log.debug`, `info`, `warn` and `error` write one line to standard error with a timestamp and level. The level comes from `VIPER_LOG` (default `info`) or `log.set_level`. The runtime formats the whole line first and writes it while holding standard error's lock, so lines from concurrent tasks do not interleave.

### D56. The HTTP Python baseline is the standard library server

docs/07 compares against a Python framework. None is installed, and installing one is a download, so the baseline is `http.server.ThreadingHTTPServer` with Nagle's algorithm turned off. A framework such as FastAPI under uvicorn would be faster than this baseline but still well behind Viper; the comparison should be rerun with one when downloads are approved.

### D57. `bytes` is a prelude struct over a string's representation

WebSocket binary messages need a type for data that is not text. `bytes` is a `@frozen` struct in the prelude holding a private (pointer, length) string whose contents need not be UTF-8; only the prelude's own methods, backed by runtime functions that never treat the data as text, look inside it. That made it a few dozen lines instead of a new built-in type through the checker and code generator: `text.encode()`, `len`, indexing (through `get_index`, D48), `slice`, `+` (through `AddNumber`), `==`, hashing, `decode() -> str?`, `hex()` and `b'...'` printing. `len(x)` now calls a `len(self) -> int` method on any struct or enum that has one. Indexing gives an `int`, not a `u8`, since sized numbers are for data layout and C (D41). Slicing syntax (`b[1:3]`) and `bytearray` are not there; a `bytes` built-in type can replace the struct later without changing programs.

### D58. WebSockets run on the connection's task; a `WebSocket` is shareable

A WebSocket endpoint is a `GET` route whose handler calls `http.upgrade`: the runtime checks the handshake, writes the 101 response, hands the connection's stream (and any bytes already read past the request) to a socket registered under an id, and the endpoint's handler then runs on the connection's own task until it returns. `WebSocket` is a `@frozen` struct of the id, the path and the path parameters, so it can be stored in a `Mutex[dict[int, http.WebSocket]]` and sent to from any task; the runtime serialises writes with a lock per socket. Reads happen on the handler's task. The upgrade request's query and headers are copied into the socket and read through `ws.query` and `ws.header`, because `http.Request` is mutable and would make `WebSocket` unshareable. A handler that returns makes the runtime send a close frame (1000), wait up to two seconds for the peer's, and close the connection; at shutdown, open sockets are closed with 1001. Close codes accepted from a peer are 1000 to 1003, 1007 to 1011 and 3000 to 4999, as Autobahn expects.

### D59. WebSocket conformance is checked by a hand-written client

The roadmap's bar is Autobahn's fuzzing client with no failures in the non-extension cases. Fetching it is a download, so `crates/lang_cli/tests/websocket.rs` sends the same kinds of frames by hand: payload lengths at the header boundaries, pings and pongs, reserved bits and opcodes, fragmentation with interleaved control frames, invalid UTF-8 in messages and close reasons, valid and invalid close codes, and messages over the size limit, each checked for the echoed data or the close code Autobahn expects. It does not cover Autobahn's timing-dependent cases (frames split into one-byte writes, slow peers) beyond what the buffering already handles, nor the performance cases. Running the real suite remains a follow-up once downloads are approved.

### D60. `@http.ws` handlers return nothing

An `@http.ws` handler takes one `http.WebSocket` and may also take the upgrade `http.Request`, path parameters and query parameters, bound by the same rules as other routes; a body parameter or `status=` is an error. It must return nothing: there is no response to make once the connection is upgraded, and errors on the connection show up as `recv` returning `None`.

### D61. Fibers are hand-written: the context switch, the stacks and the scheduler

This replaces D42 and D50. `corosensei` and `crossbeam` would have been the obvious crates, and adding them is a download, so the runtime has its own (`crates/lang_rt/src/fiber.rs` and `poll.rs`, about 1,250 lines with comments). The context switch is about twenty instructions of x86-64 assembly that save the callee-saved registers and the MXCSR and x87 control words. Each stack is an `mmap` reservation, 1 MiB by default (`VIPER_TASK_STACK`), with a guard page; only touched pages use memory. Up to 16,384 stacks are kept for reuse, which takes task creation from 3.4x to 2.7x Go's time, at the cost of keeping their touched pages. Each worker has a FIFO run queue behind a lock; an idle worker takes from the global queue, then steals half of another's, then waits in `epoll_wait`. Parking uses a permit and a state word, so that a wake-up racing with going to sleep is never lost. There is one worker per core (`VIPER_WORKERS`), and no cap on tasks: the 10,000-task cap existed to protect against thread exhaustion.

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
- **Preemption.** A fiber runs until it waits. A CPU-bound loop holds its worker; with one worker per core, as many such loops as cores stop everything else. The planned fix, a check on loop back-edges, was measured in D95 and costs a tight loop 25%, so it is still not there.
- **Clean stack overflow.** Overflowing a fiber's stack hits its guard page. Done in D95 as far as a message and the status of a panic; it still ends the whole program, since a task's stack cannot be unwound once it is full.
- **Per-worker timers and aarch64.** One timer thread serves all workers; the context switch exists only for x86-64.

### D67. Debug information is line tables, frame pointers are always on, and panics print a backtrace

Every compiled function gets a DWARF subprogram and every expression a source location; locals and types are not described yet, because describing the language's value layouts (strings, optionals, results, closures) to a debugger is its own project. That is enough for breakpoints by line, stepping, `perf report` with source lines, and backtraces. Functions keep frame pointers in release builds too, as Go's do, so `perf record -g` works without DWARF unwinding; it costs about 10% on `fib`, which is nothing but calls, and nothing measurable elsewhere. A panic prints the chain of calls in the program's own source (`Point.scaled at app.vi:6:22`), symbolised by the Rust standard library's backtrace support from the same DWARF; `VIPER_BACKTRACE=0` turns it off.

### D68. `fmt` keeps line breaks and comments, and checks itself with the lexer

The formatter scans the text itself, since the lexer drops comments, and normalises spacing, indentation and blank lines without moving code between lines: what is on one line stays on one line, and a multi-line call stays multi-line with its continuation lines indented one level. Choosing line breaks (fitting to a width) is the larger half of a formatter like Black's and was left out. The output must lex to exactly the tokens of the input, or the file is left alone and the problem reported; the repository's own test files pass that check and are stable under reformatting. The rules match the style the repository was already written in, so it changed only three files, all in aligned trailing comments.

### D69. Tests run as tasks, and `assert` is a statement

A test is a `@test` function; `viper test` appends a generated runner to the file's source (so source positions stay correct) that spawns each test as a task and asks whether it panicked, through a task method meant for the runner. This is milestone 10's task-scoped panics put to use: a failed test reports its panic and the others still run. `assert cond, message` became a statement, parsed into `if not cond: panic(message)`; `assert` is now a keyword. Tests in imported modules are not collected: tests live with the code in the file being tested.

### D70. The language server re-checks the whole program on each change

`viper lsp` reads full-document updates, runs the same front end as `check` with an index switched on (each expression's type, and where each variable, function, method, field and type it names is defined), and answers hover and go-to-definition from that index. Each change re-checks everything, which takes milliseconds for programs of the sizes in this repository; incremental checking waits until programs are large enough to need it. A missing `main` is not reported, since the file may be a module. The JSON-RPC layer and JSON parser are written by hand (about 250 lines), since adding `serde` is a download.

### D71. Dependencies: a small manifest, git or path sources, and a lock file

`viper.toml` names dependencies as `{ git = "url", rev = "tag or branch or commit" }` or `{ path = "dir" }`. A git dependency is cloned into `.viper/deps/<name>` inside the project when first needed, and the commit used is written to `viper.lock`; later builds check out that commit, so a build does not change because a branch moved. `viper fetch --update` moves each dependency to its `rev` and rewrites the lock file. `import dep.module` loads from the dependency; `import dep` loads its `lib` file; a dependency's own files import each other with its name first, like Go's import paths. There is no version solving, no registry, and no handling of two dependencies wanting different versions of a third; those wait for a real package ecosystem. The manifest format is a subset of TOML, parsed by hand.

### D72. Editor support is a grammar plus the language server

A TextMate grammar (`editors/vscode/`) covers VS Code and the many editors that read that format, and a Vim syntax file covers Vim and Neovim; both are maintained by hand against the keyword list. Neovim and Helix can start `viper lsp` with a few lines of configuration (`editors/README.md`). A VS Code extension that starts the language server needs `vscode-languageclient` from npm, a download, and is not included.

### D73. The collector grows the heap to about twice the live data

Boehm's default free-space divisor of 3 starts a collection when about a third of the heap has been allocated since the last one, which is far more often than Go's default (`GOGC=100`: collect when the heap has doubled). Setting the divisor to 1 matches Go's policy. It made the allocation-heavy HTTP endpoint 54% faster (with the response change below) and batch benchmarks 3% to 7% faster, for 10% to 25% more memory (`binary_trees`: 72 MB against 57 MB). `VIPER_GC_FREE_SPACE_DIVISOR` still overrides it; a memory-tight deployment can set 2 or 3. (The default became 2 in D86, once D84's allocation floor had taken over small heaps.)

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
  - **Stress mode.** With `VIPER_GC_STRESS`, the slow path never fills a cache, so every allocation still collects first.
- **Pointer-free structs** (numbers and bools only, in structs and enum variants) are allocated with `GC_malloc_atomic`, so the collector never scans them. Their memory is not zeroed, and `lrt_alloc_atomic_raw` says so to LLVM (`allockind("alloc,uninitialized")`). Every construction stores every field. They take the runtime call, not the inline path: a free list of objects the collector does not scan would not keep its own tail alive.
- **Escape analysis** (`lang_codegen/src/escape.rs`).
  - **What it puts on the stack.** A struct construction whose object provably never outlives the call is an `alloca` in the entry block. The analysis is flow-insensitive: each expression has a set of sources, which are construction sites and parameters.
  - **What counts as escaping.** A source escapes into returns, other objects, collections, closures, trait objects, tasks, f-strings, operators and `match`, and into any call except to a parameter that is itself known not to escape.
  - **Parameter summaries** cover the whole program and are iterated to a fixed point.
  - **Loops.** Each site has one slot per call, so a site inside a loop also needs at most one local that can hold its object: when the site runs again, that local is overwritten, so no reference to the previous object remains. `tests/run/escape.vi` covers the cases that must stay on the heap.
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
  - **Tests.** `tests/run/routing.vi` checks order, 404, 405 with `Allow`, `HEAD`, rest parameters, extra slashes, groups and late routes; it gives the same output with the old router.
- **Headers straight into the response.** `lrt_http_respond` takes the content type and writes the `Content-Type` line itself. The handler's other headers are joined into a string only when there are any.
- **A minimum allocation between collections** (`VIPER_GC_MIN_ALLOC`, default 4 MiB, Go's minimum heap).
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
- **Concatenation chains** (`lang_codegen/src/expr.rs`). `a + b + c + ...` on strings, three or more parts, evaluates the parts left to right and joins them with one call, `lrt_str_concat_n`, which allocates once at the full length. Empty parts are skipped, and a chain with one non-empty part shares it, as two-part `+` does. `tests/run/concat.vi`.
- **Not done.**
  - **Hand-written C helpers compiled to LLVM bitcode and inlined into programs (item 2).** This would add a build step that needs `clang-19`.
  - **Code-generated dict probe loops (item 3).** The exit criteria were met without either, so they wait until a profile shows the runtime's dict or UTF-8 code on a hot path again.
- **Tests.** `tests/run/dispatch.vi` covers one, three and five implementing types (the last stays indirect), upcasts through a supertrait, a method without a result, and methods with arguments.

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

The other benchmarks are unchanged. `VIPER_GC_FREE_SPACE_DIVISOR=1` restores the old behaviour.

**Not done, and why.**
- **Asking Boehm for one byte less for objects whose size is a multiple of 16.** This would avoid the extra byte where it costs a whole granule, but a pointer just past such an object would then keep the next object alive instead of this one, and LLVM may keep such pointers. Not worth that risk.
- **Turning off interior pointers.** Substrings share their parent's bytes at any offset, so the collector must recognise pointers into the middle of objects.

**Results** (full runs, [bench/README.md](../bench/README.md); the machine was about 10% slower than for phase 4, as Go's own times show):
- `binary_trees` peaks at 65 MB against Go's 40 MB, 1.6x, down from 2.1x; it is still 1.27x faster than Go.
- `json_orders` 71 MB against 59 MB.
- Every benchmark is still at or ahead of Go. `/items` is level with Go (1.01x), and the other HTTP endpoints are 1.8x to 2.1x faster.

### D87. Formatting and JSON without the Rust heap; LTO for the runtime (final plan, milestone 1)

Milestone 1 of the [final optimisation plan](10-final-optimization-plan.md). Profiles showed the runtime's use of Rust `String`s and `Vec`s on hot paths: 155 `malloc` calls per `/items` request, most of them formatting 20 floats, and 62 per `POST /orders`.

**What was done.**
- **Measuring tools** (`bench/README.md`, "Measuring a change"): `bench/http/counts.py` with the `bench/profile/count.c` shim, for exact calls and bytes per request; `bench/ab.py` for interleaved A/B runs; `VIPER_ALLOC_STATS`; a `--browser` load mode with a dozen realistic request headers.
- **LTO.** `[profile.release]` now links with fat LTO in one code-generation unit, so `lang_format` and the runtime's helpers inline into the functions programs call. A clean release build takes about 9 s longer (13 s to 22 s). Executables are a third smaller (`bench/http/server.vi`: 7.6 to 5.2 MB).
- **Floats.** `lang_format::write_float_repr` writes Python's `repr` of a float into a 32-byte stack buffer, which the builder and the JSON writer copy from. Short decimals (`|x|` from 1e-4 to 1e15 with at most nine decimals: prices, measurements) take an exact fast path: the smallest `k` for which `round(|x| * 10^k) / 10^k == |x|`. The rest go through `core::fmt`'s shortest `{:e}` into a stack buffer. The old implementation is kept in the tests, which compare the two on five million values, including the fast path's edges.
- **JSON encoding.** Strings are escaped straight into the builder, eight bytes at a time; no intermediate `Vec`.
- **JSON decoding.**
  - The reader is a 512-byte record in the generated function's frame (`lrt_json_reader_init`) instead of a `Box`. Open containers are a bit set, and the error path keeps 16 parts inline as pointers to the keys, not copies.
  - A string without escapes is copied once, from the input into collected memory, with no `Vec` and no UTF-8 check (the input is already a `str`).
  - Keys matched against struct fields are views into the input (`lrt_json_object_key_view`); dict keys, which are stored, are still copied.
  - Integers are accumulated while their digits are scanned.
  - `tests/run/json_reader.vi` pins the edge cases: error paths 36 parts deep, keys with escapes, empty keys, unfinished escapes, lone surrogates and the limits of `int`. Its expected output came from the build before this change.
- **Smaller ones.** `str.join` copies the pieces directly; `repr` of a string and plain `{x:.2f}` and `{n}` format specs write into the builder.

**Counts per request** (`bench/http/counts.py`, 256 connections):

| Endpoint | Rust `malloc` before | After | Collector calls before | After | Bytes allocated before | After |
|---|---|---|---|---|---|---|
| `/items` | 155.1 | 9.1 | 75 | 75 | 8,897 | 8,896 |
| `POST /orders` | 62.1 | 14.1 | 32 | 15 | 1,488 | 1,216 |
| `/plaintext` | 9.3 | 9.4 | 3 | 3 | 256 | 256 |

What is left on the Rust heap is the HTTP path, milestone 2's subject. The collector calls on `/items` are milestone 3's.

**Timings**, interleaved A/B against the build before this milestone (`bench/ab.py`; servers on one CPU with 64 connections unless stated):

| Measure | Before | After | Change |
|---|---|---|---|
| `json_orders` | 0.507 s | 0.225 s | 56% less time |
| `/items` | 114,600 req/s (8.7 µs each) | 181,200 req/s (5.5 µs) | +58% |
| `POST /orders` | 236,800 req/s (4.2 µs) | 307,800 req/s (3.2 µs) | +30% |
| `/json` | 411,200 req/s | 419,300 req/s | +2.0% |
| `/plaintext` | 408,200 req/s | 412,600 req/s | +1.1% |
| `/users/42` | 341,800 req/s | 347,800 req/s | +1.7% |
| `/items`, six CPUs, 256 connections | 290,500 req/s | 352,000 req/s | +21% |
| `POST /orders`, six CPUs, 256 connections | 703,400 req/s | 879,700 req/s | +25% |
| `word_count`, `sort` | | | within 1.2% |

In each row the ranges of the two sides do not overlap, except the last. Of the float gain, the stage-1 rewrite (no `String`s) and the short-decimal fast path each gave about half: the fast path alone was 11% on `json_orders` and 15% on `/items`. Peak memory fell for `json_orders` (71 to 62 MB) and is unchanged for the others.

**The gains are larger than the plan expected** (20% for `json_orders`, 10% for `/items`), which was estimated from the profiler's inclusive shares. Those are measured only to a depth of four frames, so they understated how much of each request went through `String`-building and the allocator. LTO accounts for 2% to 6% of it (measured alone during planning). The cause of the rest of the difference between estimate and result was not investigated. On one CPU, `/items` now costs 5.5 µs; Go's figure in [web-performance.md](web-performance.md) is 10.9 µs.

**Not done, and why.**
- **Matching fields from the expected position** (plan item 1.5.6). The comparison chain did not show in the profile of `POST /orders`, whose target was met without it.
- **`binary_trees` reads 3% to 5% slower** in A/B. It does not use any code this milestone changed, and its generated IR is identical. LTO is not the cause either: with and without it, from the same source, the difference was 0.4%. The program's `make` and `check` functions moved 16 bytes, from offset 32 to offset 48 within a 64-byte cache line, because the runtime linked before them changed size. This is the layout effect the handoff describes, not a change in work. Aligning hot generated functions is noted for milestone 4.

### D88. One read, two coarse clock reads, and no copies per request (final plan, milestone 2)

Milestone 2 of the [final optimisation plan](10-final-optimization-plan.md): the HTTP connection path.

**What was done.**
- **The readiness protocol** (`poll.rs`). An edge used to both set a source's `ready` flag and wake its waiter, so the woken task's next wait returned at once, and every request cost a `recv` that found nothing. Now an edge wakes the waiter if there is one and otherwise sets the flag, decided under the waiter's mutex on both sides. Timeouts are reported by the deadline sweep through a `timed_out` flag, so a wait reads no clock.
- **No read of a drained socket** (`Socket::read`). After a read returns less than it asked for, the socket was empty, and with edge-triggered polling any later bytes produce a new edge; the next read waits for it first. Plain sockets only (not TLS), and never after the end of the stream or a full buffer.
- **The clock.** I/O deadlines are on `CLOCK_MONOTONIC_COARSE` (2 ns against 20 ns), computed only when a task is about to wait. The `Date` header is formatted once a second per thread, from the coarse real-time clock, instead of being cloned under a global mutex.
- **Requests parsed in place.** The request is offsets into the connection's buffer: a read cursor instead of `drain`, the end of the head found with `memchr` and resumed where the last search stopped, headers kept as ranges (32 inline), the four the server needs recognised in the same pass, the method a static string, the path decoded only if it has a `%`, the remote address formatted only when asked for, and the body a range. The accessors copy once, straight into collected memory; `lrt_http_req_method` copies nothing.
- **Responses into a reused buffer.** `lrt_http_respond` writes the status line (from a table), headers and body into the connection's output buffer, which is sent when the connection is about to wait for more input or holds 16 KiB. Pipelined requests' responses share a `send`.
- **Tests** (`lang_cli/tests/http.rs`): a head and a body arriving in pieces, 40 headers in mixed case, an escaped path, 300 pipelined requests answered in order across several writes, a panic in the second of two pipelined requests, and a client that closes its side after sending.

**Counts per request** (`bench/http/counts.py`, 256 connections):

| | Before | After |
|---|---|---|
| `recv` (of which found nothing) | 3.0 (2.0) | 1.0 (0) |
| `clock_gettime` | 5.0, precise | 2.0, coarse |
| Rust `malloc`, `/plaintext` | 9.4 | 0.5 |
| Rust `malloc`, `/plaintext` with a dozen browser headers | 36.1 | 0.6 |
| Rust `malloc`, `POST /orders` | 14.1 | 0.2 |

**Timings**, interleaved A/B on one CPU with 64 connections, against milestone 1:

| Endpoint | Before | After | Change |
|---|---|---|---|
| `/plaintext` | 417,600 req/s (2.39 µs) | 526,400 req/s (1.90 µs) | +26% |
| `/json` | 414,700 | 525,700 | +27% |
| `/users/42` | 337,000 | 419,800 | +25% |
| `POST /orders` | 294,900 | 343,700 | +17% |
| `/items` | 173,800 | 186,400 | +7% |
| `/plaintext`, browser headers | 306,100 | 445,500 | +46% |
| `/users/42`, browser headers | 267,400 | 359,700 | +35% |

**On six CPUs with 256 connections** (`bench/ab.py --server`), `POST /orders` gained 16% (845,000 to 977,000 requests a second) and `/users/42` 6%, while `/plaintext`, `/json` and `/items` stayed level (1.10 million, 1.08 million, 340,000). The server is not the limit there: it kept 4.8 to 5.1 of its six CPUs busy before and after, and its CPU time per request fell 5% to 7% (`/plaintext`: 4.66 to 4.41 µs, kernel time included). The load generator and the kernel's loopback work on the other six CPUs set those rates. The one-CPU figures above are the measure of the server.

**Not met: browser headers within 10% of the bare rate.** With a dozen headers (about 600 bytes), `/plaintext` runs at 85% of its bare rate (445,500 against 526,400). The parser no longer allocates per header, so what is left is the kernel copying the larger request and one pass over its bytes (UTF-8 check, line search, header split). Before this milestone it was 73%.

**Idle WebSockets** (`bench/ws_idle/`, 50,000 connections): 19.6 KiB each with milestone 1's runtime, 18.8 KiB now. The handoff's 15.5 KiB was measured before milestone 1. **LTO is what added the 4 KiB:** the same source built without LTO measures 14.8 KiB, one stack page less per parked connection. Two frames on that stack were found inflated by inlining and are now kept out of line, which shrank the serving closure's frame from 5.9 to 3.3 KiB: rustls's session set-up (`start_tls`) and `read_request`. Neither moved the measurement, so the extra page is deeper, between the handler and the park; the plan's half-day bound for this item ran out there. LTO stays: it is worth 2% to 6% of speed, against 4 KiB per idle WebSocket (about 1 GB more for 250,000 idle connections). Finding the frame is noted for milestone 4.

**Not done.** `writev` for large bodies (optional in the plan): no benchmark needs it. A single atomic state word in place of the waiter's mutex: `IoSource::wait` no longer shows in profiles.

### D89. Runtime allocations from per-thread lists; buffers sized once; parallel marking (final plan, milestone 3)

Milestone 3 of the [final optimisation plan](10-final-optimization-plan.md): allocation and the collector.

**What was done.**
- **A stress mode for the free lists** (`GC_STRESS=cache`), built first: the lists stay in use but hold at most two objects, and every refill collects. An object on a list that the collector cannot see is then reclaimed and handed out again within a few allocations. Every program in `tests/run` now also runs in this mode. Checked by planting such a bug (the lists' block on the Rust heap): 16 of 46 programs failed in this mode and none without it.
- **Runtime allocations of scanned memory from per-thread free lists.** `gc::alloc` below 256 bytes pops from a per-thread cache of the same shape as generated code's, refilled by the same `lrt_alloc_slow`. It is a separate cache held in a Rust `thread_local!`, read only inside an `#[inline(never)]` function that never parks.
- **The JSON output builder sized from the last result.** A global per encoded type holds the length of its last output (read and written with `unordered` atomics), and the next encoding reserves that much, up to 64 KiB. Builder buffers are rounded to 16n - 1 bytes, which the collector's extra byte makes a whole number of granules.
- **Comprehensions of known length presized.** One `for`, no `if`, over a list or a `range` with step 1: sema hoists the source into temporaries and starts the list with that many slots (`Builtin::ListWithCapacity`, at most 2^20). List literals store their items straight into the buffer and set the length once. `tests/run/comprehension_sizes.vi` covers empty, reversed and filtered cases and the order of evaluation.
- **Parallel marking for batch programs.** The collector gets up to four marker threads (`GC_set_markers_count`), which start only once threads are allowed. A heap-growth callback sets a flag at 32 MiB (`PARALLEL_MARK_HEAP`), and the allocation slow path then calls `tasks::allow_threads()`, as the first `spawn` does. Four against six markers, and an 8 MiB threshold, made no difference.

**Tried and dropped: a cache for unscanned memory** (plan items 3.3 and 3.4). Built as the plan described it: per size, an array of pointers in a scanned block, refilled from `GC_generic_malloc_many` with collection disabled while the list is copied. Planting a bug showed the stress mode catches mistakes in it (4 programs failed). But it gained nothing: `json_orders` 0.4% slower with it, `/items` 2.1%, `POST /orders` 1.0%, and `sort` 5% to 10% slower (handing objects out in descending address order explained about half of that). Boehm's own thread-local lists for unscanned objects are already that fast. It was removed, and with it the reason for inline allocation of pointer-free structs.

**Counts per request** (`bench/http/counts.py`):

| `/items` | Before | After |
|---|---|---|
| `GC_malloc` calls | 46 | 0 |
| `GC_malloc_atomic` calls | 28 | 22 |
| Bytes allocated | 8,880 | 5,328 (-40%) |

**Timings**, interleaved A/B against milestone 2:

| Measure | Before | After | Change |
|---|---|---|---|
| `/items`, one CPU | 201,400 req/s (4.97 µs) | 246,000 req/s (4.07 µs) | +22% |
| `/items`, six CPUs, 256 connections | 348,300 req/s | 572,800 req/s | +64% |
| `/users/42`, one CPU | 423,100 | 438,800 | +3.7% |
| `POST /orders`, one CPU | 350,700 | 361,500 | +3.1% |
| `/plaintext`, `/json`, one CPU | | | +1% |
| `binary_trees` | 0.675 s | 0.577 s | -14.5% wall time |
| `json_orders` | 0.223 s | 0.209 s | -6.2% |
| `word_count` | 0.610 s | 0.582 s | -4.6% |
| `sort`, `sieve` | | | within 1.2% |

**Why six CPUs gained most.** With several threads, every `GC_malloc` takes the collector's allocation lock. On `/items` the runtime made 46 of those calls per request, on six CPUs at once; now it pops from its own list. Peak memory: `json_orders` 62 to 57 MB, the others unchanged.

**Not met: `binary_trees` at 0.55 s.** It reached 0.577 s. The plan's 20% came from the earlier measurement with all of the machine's CPUs marking; capped at four, and only from 32 MiB, it gains 14.5%, and more markers or an earlier start did not help.

### D90. Typed dict entry points, short-key hashing, inline builder pushes, one-compare indexing (final plan, milestone 4)

Milestone 4 of the [final optimisation plan](10-final-optimization-plan.md): generated code.

**What was done.**
- **Dict entry points by key type.** For `str` keys, `lrt_dict_find_str` and `lrt_dict_slot_str` take the key as pointer and length; for `int` keys, `lrt_dict_find_int` and `lrt_dict_slot_int` take it by value. Each has its hash and comparison inline: no spill to the stack, no check of the descriptor on every call and every probe. The compiler picks the entry point at every site (`Codegen::dict_entry`); other key types, and whole-dict operations (copy, equality, removal), keep the generic functions, which compute the same tags.
- **Hashing.** `str` keys use the string hash directly, without a second SplitMix pass over a hash that is already mixed; `int` keys keep it. Strings of up to 16 bytes are hashed from at most two overlapping loads, with no loop and no copy into a zeroed buffer.
- **Builder pushes inline.** `lang.builder.push`, an always-inline function in each module, compares the length with the capacity, copies and bumps the length, and calls `lrt_builder_push` only to grow. With a constant length, as for every literal piece of an f-string or a JSON encoding, the copy becomes a few stores.
- **List indexing** takes one unsigned compare on the common path, `0 <= i < len`; negative indexes and the panic are out of line.
- `tests/run/dict_keys.vi` checks the typed paths against a linear list of pairs and against the generic paths: `str` keys of every length from 0 to 40, ASCII and not, keys sharing bytes with other strings, removals, copies and equality; and negative, consecutive and extreme `int` keys. Its expected output came from the compiler before this change.

**Timings**, interleaved A/B against milestone 3:

| Measure | Before | After | Change |
|---|---|---|---|
| `word_count` | 0.582 s | 0.348 s | -40% |
| `sieve` | 0.053 s | 0.049 s | -7.8% |
| `json_orders` | 0.209 s | 0.195 s | -6.4% |
| `/items`, one CPU | 246,800 req/s | 292,800 req/s | +18.6% |
| `/items`, six CPUs | 546,700 | 608,500 | +11.3% |
| `/users/42`, one CPU | 434,100 | 443,500 | +2.2% |
| Other HTTP rows, `sort`, `binary_trees` | | | within 1.5% |

Dict work fell from 49% of `word_count` to 31%: `lrt_dict_slot_str` (with the probe inlined) 23.8%, hashing 7.3%.

**Tried and not kept.**
- **Aligning generated functions to 64 bytes** (D87 found `binary_trees` sensitive to where its functions fall in a cache line). `binary_trees` gained 4.9% and `word_count` lost 5.4%, the rest within 2%: alignment moves the luck around rather than removing it.
- **A `--cpu` flag** (`native` uses the host's CPU name and features). On this machine it made `word_count` 6.3% and `sieve` 4.3% slower and nothing faster, so the default generic x86-64 stays and the flag was not added.

**Not done.**
- Hash bits in the index slots (optional in the plan): `word_count` beat its target four times over without it.
- Merging neighbouring literal pieces in the JSON encoder at compile time: with pushes inline, each piece is already a few stores.
- `bool` list elements as `i8`: `sieve` got its gain from the index path, and its IR showed no blocked vectorisation worth the change.

### D91. Syntax gaps: one-line blocks, raw strings, `del`, constant expressions, f-string conversions, `print` keywords, `match` expressions (release polish plan, milestone 1)

Milestone 1 of the [release polish plan](11-release-polish-plan.md). Each item was found by compiling a small program that a Python programmer would write in the first hour.

**What was done.**
- **One-line blocks.** After the `:` of `if`, `elif`, `else`, `while`, `for`, `with`, `def` and `case`, one simple statement may follow on the same line. A compound statement there is an error that says to start a block on the next line (`inline_block` in the parser). The formatter needed no change, since it keeps line structure (D68). A `match` whose cases all failed to parse no longer reports "needs at least one `case`" on top of the real error.
- **Raw strings.** `r"..."`, `r'...'` and the triple-quoted forms keep backslashes as written; a backslash before a quote or another backslash keeps both characters and does not end the string, as in Python. `rf"..."`, `b"..."` and `rb"..."` stay errors, now with advice. The TextMate (VS Code) and Vim grammars highlight `r` strings.
- **`del`.** `del xs[i]` is `xs.pop(i)` with the value dropped; `del d[k]` is a new `DictDelete` operation that panics with the missing-key message used for `d[k]`. `del` of a variable, a field, a slice or anything else is an error saying what to write instead. `del` is a keyword.
- **Constant expressions.** `const A = 4 * B + 1` works, with other constants in any order and across files. The value is checked as an ordinary expression, then folded to a literal over the HIR (`check/consts.rs`), mirroring the code generator's semantics for each operator: wrapping integer arithmetic, floor division and modulo, shifts of 64 or more, the same float operations. Each constant is checked the first time something needs it, so order does not matter, and a cycle is reported with its chain. Calls, collections and f-strings are errors that say so. `tests/run/const_expressions.vi` compares twenty folded constants with the same operations done at run time.
- **f-string conversions.** `{x!r}` and `{x!s}`, with an optional spec after (`{x!r:>10}`), which pads the text as for a string. `!a` and other letters are errors.
- **`print` keywords.** `sep`, `end` and `flush`, with Python's meanings. A call without keywords compiles as before; one with keywords uses a new `PrintWith` operation and `lrt_builder_print_flush`. `file=` is an error pointing to `io.eprint`.
- **`match` expressions.** As the whole right-hand side of an assignment (with or without a type hint) or of `return`, each case's value on its own line. The checker lowers it to a `match` statement whose cases store into a hidden local, inside a block that yields the local (`Checker::match_expr`). The type is the expected type, or the first value's, as for `a if c else b`; a panicking case takes no part and a `None` case makes the result optional. A `match` expression anywhere else is an error that says to assign it first. The cases must be exhaustive: there is no value to fall back on.
- **Help messages.** The stale "arrives in milestone 3" for `int("42")` now says to use `int.parse`. Reading a field of an optional says to test for `None` or `.unwrap()`. A misspelt keyword in a struct call (`User(nmae="a", age=1)`) gives one error with the suggestion, not also a missing field. `open`, `map` and `filter` get the Python-to-Viper advice. "Did you mean" now ignores case and counts a swapped pair of letters as one change, so `Strr` finds `str` and `nmae` finds `name`.

**Differences from the plan.**
- **Bytes literals** (`b"..."`) were optional and are not done: a `str` literal cannot hold the invalid UTF-8 that `\xff` makes, so they need a new literal kind through the checker and code generator. The error says to use `"...".encode()`.
- **The tree-sitter grammar** was not changed. Its `parser.c` is generated by the `tree-sitter` command, which is not installed here, and fetching it is a download.

**Tried and not kept.** Desugaring `x = match ...` into a statement `match` in the parser: it cannot carry a type hint on `x`, since each case would redeclare it, and a hint is how an empty list or `None` case gets its type.

### D92. Methods of the built-in types written in the prelude (release polish plan, milestone 2)

Milestone 2 of the [release polish plan](11-release-polish-plan.md). A method of `str`, `list` or `dict` used to need a case in the checker's signature table, a `Builtin` operation, a case in the code generator and usually a runtime function, which is why there were so few.

**The mechanism.** The prelude (`crates/lang_sema/src/prelude.lang`) defines ordinary functions named `__<type>_<method>`, where the type is `str`, `list`, `dict`, `tuple`, `frozendict`, `int`, `float`, `bool`, `option` or `result`. When `value.method(...)` finds no method of that name in the compiler's tables, or none that takes those arguments (`strip()` is the compiler's, `strip("x")` the prelude's), `builtin_method` looks for `__<type>_<method>` and calls it with the value as the first argument (`call_prelude_method`). The function may be generic, with bounds (`def __list_count[T: Eq](items: list[T], value: T)`): `call_generic_with` takes the already-checked receiver, infers the type parameters from it and from the other arguments, and instantiates the function like any generic. A function can be an `@extern` of a runtime function, which is a direct call. Methods the compiler defines keep priority, so no existing fast path changed, and release executables are the same size as before, since LLVM drops what is not called (a debug executable is 1% larger).
- **Messages name the method, not the function.** An arity or type error says `str.count takes 1 argument but 0 were given`, and "did you mean" draws on the compiler's methods and the prelude's together. The prelude's source is never shown as a "defined here" label. A bound that fails says `list.count requires Eq of T`.
- **A panic points at the caller.** A prelude function whose last parameter is named `__at` is given the text of the call's location (`file:line:col`, which `lrt_panic` takes), and panics through `__panic_at(message, __at)`. Without it a panic in the prelude would say `<prelude>:96:13`. `ord` and `chr`, which are plain functions, get it the same way.
- **The `__` names are not hidden.** They are visible to user code, as `__bytes` always was, because the code that `server.mount` and `viper test` generate is checked in the user's scope and calls some of them. They are reserved by convention.

**What was added.**
- `str`: `count`, `rfind`, `index`, `partition`, `rpartition`, `rsplit`, `split(sep, limit)`, `split_lines`, `remove_prefix`, `remove_suffix`, `is_digit`, `is_alpha`, `is_alnum`, `is_space`, `is_upper`, `is_lower`, `title`, `capitalize`, `zfill`, and `strip`, `lstrip` and `rstrip` with the characters to remove. The byte-level work is in `lrt_str_count`, `lrt_str_rfind`, `lrt_str_class`, `lrt_str_title`, `lrt_str_capitalize` and `lrt_str_strip_chars`; the rest is in the language.
- `list`: `count`, and `items * n` and `n * items` (`__list_repeat`, called from the `*` operator).
- `dict`: `get_or_insert`, `update`, `copy`, `pop_or`.
- Functions: `ord`, `chr`, `reversed`, and the compiler's `list(x)` for a list, a string, a dict's keys and `range(...)`. `list(range(...))` is a comprehension over the range, so it compiles to the same counting loop. Python's names for these methods (`splitlines`, `isdigit`, `setdefault`, ...) get "the method is called `split_lines`".
- AST nodes are now `Clone`, for `list(range(...))` and later for generated code.

**Decisions.**
- **`str.index` panics** when the substring is absent, as Python's raises and as `list.index` here panics, rather than returning an optional as the plan first said: `find` already answers "maybe", and having `index` mean different things on a list and a string would surprise. `index(): the substring is not in the string`.
- **Unicode classes.** `is_digit` is true only for ASCII digits, because Rust's standard library has no test for Unicode decimal digits and ASCII is what `int.parse` accepts; the others follow Rust's definitions of alphabetic, alphanumeric, whitespace, upper and lower case. `is_upper` and `is_lower` need a cased letter and none of the other case, as in Python. `title` treats a cased letter as part of a word (`it's` gives `It'S`, as in Python). `split_lines` splits at `\n`, `\r\n` and `\r` only, not at the other separators `splitlines` knows. `zfill` counts characters, `find` and `len` count bytes (D22).
- **`reversed` and `list(...)` give lists**, not iterators, since there are no lazy iterators yet.
- **`list * int` evaluates the list first**, then the count, whichever is written first.

**Not done.** Prelude methods for `Option`, `Result`, `int` and `float`: the machinery handles them (the prefixes exist) and nothing needed one.

### D93. Sets: a dict without values (release polish plan, milestone 3)

Milestone 3 of the [release polish plan](11-release-polish-plan.md). `set[T]` is the type `dict[T, None]` inside the compiler, written `set[T]` in programs and in messages (`Types::set`, `Types::set_elem`). The dict's descriptor gives its value size as 0, so an entry is a tag and the element, and everything that works on a dict's keys works on a set unchanged: `in`, `len`, `for`, equality, copying, the collector's descriptor, `sorted`, `sum`, `min` and `max`. Writing `dict[K, None]` in a program is still an error ("`None` cannot be used as a type here"), so a dict with `None` values exists only as a set.

**What was done.**
- **Syntax.** `{a, b}` and `{x for x in xs if c}` (parser: `ExprKind::Set` and `SetComp`), `set()` with the type from the context, `set(x)` of a list, string, dict's keys, set or `range(...)` (it builds the list the way `list(x)` does and calls the prelude's `__set_of`). `{}` stays an empty dict, and `x: set[int] = {}` says to write `set()`.
- **Operations.** The compiler has two new operations, `SetAdd` (a slot made for the element, nothing stored) and `SetDiscard` (removes and says whether it was there), and `clear` is the dict's. The rest is in the prelude, written in the language: `remove`, `pop`, `copy`, `update`, `union`, `intersection`, `difference`, `symmetric_difference`, `is_subset`, `is_superset`, `is_disjoint`. The operators `|`, `&`, `-` and `^` on two sets of one type call the same functions. A set literal and a comprehension are blocks that make an empty set and `add` to it.
- **Printing** is `{1, 2}` and `set()` when empty, and inside containers elements print as in a list. **Equality** compares the elements (the value comparison is a function that always says equal). **JSON** writes a set as an array and reads an array into a set, adding repeated elements once; a `set[T]` field with `@derive(Json)` needs `T` to be JSON-capable.
- **What a set cannot do** is checked where the dict code would have allowed it: indexing, assigning to an element, `del`, `keys`, `values`, `items`, `get`, `pop(key)`, `frozendict(s)`, a `<` or `<=` (the message says `is_subset`), `+` (the message says `|`), use as a key or as an element of another set (not hashable), sending to a task (not shareable; `Mutex(set)` works).
- **Fixed on the way:** `{k: None for ...}` made a dict the code generator could not handle and crashed the compiler; it is now an error that says what to write.

**Differences from the plan and from Python.**
- **Iteration order is insertion order**, as for dicts; Python's sets have an order of their own. Programs should not depend on either.
- **`discard` returns a `bool`**, whether the element was there.
- **The methods take sets**, not any iterable: `a.union(set(items))`. `update` likewise.
- **`pop` removes the element added first** and gives `T?`, `None` for an empty set.
- **`<`, `<=`, `>` and `>=` are not subset tests**; use `is_subset` and `is_superset`. The in-place operators `|=` and the like are not there.
- **`frozenset` is not done.** The plan made it optional; an immutable set would follow `frozendict`'s cases (about 27 sites) and nothing needed it. A set is shared through a `Mutex`.
- **A dict entry of a set takes 8 bytes for its tag**, not counting the element: no space for a value, as the plan hoped.

### D94. Generic methods and type patterns on trait objects (release polish plan, milestone 4)

Milestone 4 of the [release polish plan](11-release-polish-plan.md).

**Generic methods (amends D33).** `def map[U](self, f: def(T) -> U) -> Box[U]` in `impl[T] Box[T]` works, and so do type parameters on a method of a plain `impl Counter:`, with bounds (`[A: Display]`) and on methods without `self` (`Counter.of(items)`).
- **Mechanism.** In each instance of an `impl` block, `collect_inherent` makes the method a template, like a top-level generic function's (`declare_generic_method`, a `GenericFn` that remembers `MethodOf`: the type, the block's bindings and how its bodies are checked). `value.method(...)` finds it in `generic_methods` and calls `call_generic_with`, which infers the method's parameters from the arguments (lambdas last) and the expected type, checks the bounds, and instantiates it once per set of type arguments, with the block's type parameters bound as in the block. A method's body is checked once, reported, in the pattern of its block; the instances are checked quietly and compiled.
- **The method's type parameters are made once**, however many instances of the block there are. Made per instance, `Box[U]` in the return type would be a new type, with its own `map`, with a new `U`, without end (the first attempt overflowed the compiler's stack).
- **Type parameters are inferred, not written.** `box.map[str](f)` is not parsed as an explicit instantiation; when a parameter cannot be inferred the error says to add a type hint or an argument that has the type.
- **Trait methods cannot have type parameters**: a trait can be a type (a trait object has one function per method), so the error (E0325) says so, instead of the old "unknown type `T`".
- The cascade the plan mentioned, a method that fails to declare giving "has no method" errors at every call, was only reachable through this one error, which is gone.

**Type patterns on trait objects.** `case Circle(c):` on a `Shape` binds `c` as the `Circle`; `case Circle():` only tests; `Circle(_)` and `|` alternatives work, with guards, and through supertraits (an upcast object's vtable also starts with the descriptor). The type must be a non-generic struct or enum that implements the trait (the error says a `Shape` is never one when it does not). Generic types are an error rather than `case Box[int](b)`, which the pattern syntax does not parse; none of the existing programs needed it. A pattern with two names, or two alternatives binding one name to different types, is an error. The `match` is never exhaustive without a `case _:` or a binding name, and the error says why. The code generator compares slot 0 of the object's vtable, which every vtable fills with the type's descriptor, with `desc(ty)`: one load and one compare, no calls. `SelfType`, `self` in a trait's default method, still cannot be matched.

**Not done: empty collections typed by their first use (4.3, optional).** `xs = []` followed by `xs.append(1)` is still E0315 with the hint to write `xs: list[int] = []`. The plan said to stop if the work went beyond the local table, the assignment, the method checker's resolving forms, argument checking and one HIR patching walk. Both designs seen do. A type variable for the empty literal would have to be carried through every operation on the local until the first use fixes it, and patch every HIR node typed with it afterwards (there is no mutable HIR visitor to write that walk with). Checking each function twice, the first time quietly, to learn from the first use what the literal's type is and the second time as if the user had written the hint, avoids patching but needs a placeholder type that every later check tolerates, and a hook in each resolving form (`append`, `insert`, `extend`, `add`, `d[k] = v`, `return`, arguments, typed assignments). That is a project of its own, and a half-working version would be worse than the clear error, which already says what to write.

### D95. Robustness and tooling: stack overflow, completion, imports, error reference, smaller executables (release polish plan, milestone 5)

Milestone 5 of the [release polish plan](11-release-polish-plan.md).

**A message for stack overflow (amends D66).** Running out of stack used to end a program with "Segmentation fault" and status 139. `lang_rt/src/overflow.rs` installs a `SIGSEGV` and `SIGBUS` handler, on a 64 KiB alternate stack made for the main thread and for each worker, since the thread's own stack is the one that is full. If the faulting address is in or just beside the running fiber's guard page (`fiber::guard_window`, which reads only the thread-local current fiber), or near the end of the main thread's stack (found with `pthread_getattr_np` at start-up, and near the interrupted stack pointer), it writes `panic: stack overflow` (with the task stack's size and `VIPER_TASK_STACK` for a task), flushes what the program had printed if no one holds the output lock, and calls `_exit(101)`, the status of a panic. Anything else restores the default action and returns, so a real fault faults again; a signal sent from outside (`kill -SEGV`) is raised again. It ends the whole program even for a task: the panic unwinding of D64 cannot run on a stack that is full. `tests/run/stack_overflow_main.vi` and `stack_overflow_task.vi` run in all five modes, including the collector's stress modes.

**Preemption: measured and not done (optional).** A flag read at the top of every loop iteration, with a call to give way when the timer thread has set it, was built as a prototype and measured against the same build without it (`bench/ab.py`, seven interleaved rounds): `sieve` 25.1% slower, `word_count` 2.2% faster, `sort` 0.2% faster, `json_orders` 2.5% faster (the last three are noise). The plan's limit was 2%. The flag was already a plain global, not thread-local, so there was nothing cheaper to try for the read; a counter that checks every few hundred iterations costs about the same per iteration, and a check only in loops with calls leaves exactly the tight loops that hold a worker. The prototype was reverted. A CPU-bound handler still holds its worker until it waits; the scheduler's one worker per core and the advice to keep handlers short stand.

**Completion in the language server (amends D70).** `completionProvider` with `.` as a trigger. After a dot the server checks the document with the half-typed member removed (`user.na` becomes `user`, which parses), finds the type of the expression that ends at the dot in its index, and lists that type's members: a struct's public fields and its inherent, generic and trait methods, or the compiler's and the prelude's methods of a built-in type, or a module's `pub` names. Elsewhere it lists the locals found in the tokens of the enclosing function (parameters, assignments, loop variables, `as` names, which are there even when the text does not parse), the file's declarations, the built-in functions and types, and the keywords. Each item has a kind and, where there is one, a signature. The data comes from `lang_sema::Completions`, collected by the checker alongside the index. Scripted-session tests cover a struct, a `str`, a list, a module, names in scope, and a document that does not parse.

**Warnings.** `W0004`, an `import` of a name the file never uses. It looks at the file's tokens (and the code in f-strings' braces), not at what the checker resolved: a name counts as used if it appears anywhere outside the import lines, so a variable of the same name hides an unused import, but a used import is never reported. No program in the repository got a warning it should not have. The standard library's files are not checked. The plan's second warning, an unused private function, was optional and is not done: the route adapters that `server.mount` generates call methods from source the user did not write.

**Error reference.** `docs/errors.md` has a section for every code, with the meaning, a real message taken from the ui tests, and what to do, and two tests (`crates/lang_cli/tests/errors.rs`) fail when a code has no section or no ui test produces it. Nine codes had no test; `tests/ui/lexical_errors.vi` and `definition_errors.vi` cover them. `viper explain E0315` prints a section; the reference is part of the compiler (`include_str!`).

**Smaller executables.** The 4.4 MB of a debug executable and 5.2 MB of the benchmark server were almost all DWARF from the runtime archive (the standard library, `rustls`, `ring`, `regex-lite`); the program's own debug information is 5 KB. The linker step now runs `objcopy --strip-debug` on a copy of the archive in the build's temporary directory (50 ms), leaving the program's object, and with it the line tables behind panic backtraces and `perf`, as it was. The benchmark server falls from 5.24 MB to 1.28 MB and `fib` from 4.46 MB to 0.50 MB. `<PREFIX>RUNTIME_DEBUG` keeps the runtime's debug information for debugging the runtime; with no `objcopy` nothing is stripped. `crates/lang_cli/tests/executables.rs` checks the size and that backtraces still name the program's functions and lines.

**Re-running the probes of the plan's appendix.** Every row of Appendix A that the plan set out to fix now compiles and gives Python's answer, except by design: comparison chains (`1 < x < 3`), `*args`, assigning a captured variable, and generic traits. Bytes literals, `frozenset`, preemption, and empty collections typed by their first use are the things the plan hoped for that are not there, each with its reason in D91 to D95.

### D96. Documentation arguments on route annotations, and docstrings kept (OpenAPI plan, milestone 1)

Milestone 1 of the [OpenAPI plan](12-openapi-docs-plan.md).

**Annotation values.** An annotation argument can now be `True`, `False` or a list in brackets (`ast::AnnotationValue::Bool` and `List`), as well as a name, a string or a number. Only the route annotations use them so far; the other annotations still reject what they do not expect with their own messages. The formatter keeps tokens and needed nothing; the tree-sitter grammar already parses annotation arguments as an ordinary argument list.

**Route annotations.** `tags=["todos"]` (strings, not empty), `deprecated=True` and `errors=[404, 409]` (statuses from 400 to 599, sorted and with repeats removed; not on `@http.ws`) are accepted and stored on the route, in `RouteDoc`. They change nothing about how requests are served. Any other keyword is the existing `E0340`; `summary=` and `description=` get a help line saying they come from the docstring. `tests/ui/route_annotation_errors.vi` has each wrong form.

**Docstrings.** A handler's docstring is the first statement of its body when that is a plain string (the parser already kept it as an expression statement; it was just never read). It is cleaned as Python's `inspect.cleandoc` does, and split at the first blank line: the first paragraph, joined into one line, is the summary, and the rest is the description, kept as Markdown. Module and struct docstrings are still skipped by the parser; nothing needs them yet.

### D97. The OpenAPI document: schemas and operations at compile time, joined at run time (OpenAPI plan, milestones 2 and 3)

Milestones 2 and 3 of the [OpenAPI plan](12-openapi-docs-plan.md), built and committed together because milestone 2's output can only be tested through milestone 3's document.

**Only when asked for.** Before checking routes, the checker scans the tokens of the program's own files for a `.` followed by `docs` or `openapi_json`. Only then does `server.mount` generate the documentation calls; otherwise the generated adapters are exactly as before, so the benchmark servers and every program that does not document itself compile to the same code. The scan can be too generous (another type's `docs` method), never too strict.

**Compile time** (`check/openapi.rs`). For each mounted route, the operation object is written as JSON text from the `Route` the checker already has: parameters from the bindings (path, query with its literal default, header, cookie), the request body, the success response from `status=` and the reply kind, a 400 whenever a binding can fail, the `errors=` statuses, tags (default: the mounted type's name), the docstring's summary and description, `deprecated`. Every `@derive(Json)` type the operations reach becomes a component schema written from the checker's struct and enum tables, following the encoder in `lang_codegen/src/json.rs`: all fields are written, a field is required unless it has a default or is optional, `@json(name=)` and `@json(strict)` are honoured, an enum with fields is `oneOf` its variants tagged by `type`. A component is named by the type as written, with a generic type's arguments joined by `_`; two types with the same name are the new error `E0341`, and so is a type called `Error`, which names the error responses' body. The generated adapter hands the text to the server with `__server.__document(method, __prefix + path, operation_id, operation, ws)` and `__server.__schema(name, schema)` (public in name only: generated code cannot reach private members; completion hides `__` names).

**Run time** (`http.lang`). `server.docs(path, title, version, description, servers, hide_ws)` registers `GET path/openapi.json` and `GET path/openapi.yaml` as ordinary routes (so middleware applies); `server.openapi_json(...)` returns the document as text. The document is joined from the recorded text on the first request and kept until another route is registered, so `docs` may come before or after `mount`. It lists the routes in registration order, so a route mounted on a group gets the group's prefix, and the docs routes are left out. Call-style routes are listed with their method, path parameters and a `default` response. Operation ids are made unique at run time (`raw`, `raw_2`) when the same type is mounted twice; the compiler already qualifies a method name two types share (`Users_list`). WebSocket endpoints are listed (`101`, `x-websocket: true`) only with `hide_ws=False`. The YAML form is written from the parsed JSON: block mappings and sequences, with strings double-quoted (JSON escapes are valid YAML) unless they are plain words.

**Tests.** `tests/openapi/schemas` documents every kind of JSON type (generics, a recursive type, both kinds of enum, sets, tuples, sized integers, `json.Value`, a strict struct, a renamed field, a type mounted twice, a group prefix) and prints values of each as `json.encode` writes them; `crates/lang_cli/tests/openapi.rs` validates each against its schema with a JSON Schema subset validator written in the test (nothing could be downloaded), checks the validator rejects wrong values, and compares the document with a golden file. The same test starts the todo service, fetches `/docs/openapi.json` and `/docs/openapi.yaml`, checks the YAML says the same, and sends requests: every status the service gives is declared with a body that fits, and every status declared is produced by some request. `tests/run/openapi_json` uses the document in process.

**Limits.** A generic struct cannot be a handler's return or body type directly (the route checker runs before generic instances are set up; this predates the documentation) but can be a field. Field defaults are documented only when they are literals.

### D98. The API documentation page: our own viewer, served from the runtime (OpenAPI plan, milestone 4)

Milestone 4 of the [OpenAPI plan](12-openapi-docs-plan.md). `server.docs(...)` serves the page as well as the document unless it is given `ui=False`.

**Four plain files, no build step.** `crates/lang_rt/assets/viewer/` holds the page template, a style sheet, `core.js` (everything that works something out: references, examples from schemas, the request a form describes, `curl` commands, the Markdown of descriptions, with no DOM so that `node --test` can test it) and `viewer.js` (the DOM). They are plain JavaScript and CSS with no framework, 44 KB in total. The page builds everything with DOM calls; the only HTML made from text is `markdown`'s, which escapes first and allows `http`, `https` and relative links only. "Try it out" sends with `fetch` to the document's first server, or the page's own origin. Credentials typed into "Authorize" stay in memory.

**Served from the runtime.** `include_str!` puts the files in `lang_rt` (`docs.rs`), and the `http` module reaches them through `lrt_docs_asset(index)`, as strings that need no copy. Only `docs` calls it, so an optimised program that never calls `docs` has no reference to them, and `--gc-sections` leaves them out. `docs` fills the template's placeholders once (the title HTML-escaped, the document's URL, whether "Try it out" is shown), and hashes each file once for its `ETag`. Every response says `Cache-Control: no-cache`, so a browser revalidates and gets `304` while the file is unchanged, and an upgraded server is never shadowed by an old cached viewer. The page has no inline script or style, so its policy can be `default-src 'self'` (with `connect-src *` for "Try it out"); the template passes settings in `<meta>` tags instead.

**Tested at three levels.** `core.test.js` (run when `node` is installed), HTTP tests of each file's headers and `304`, and, when an environment variable names a Chromium, a headless load that must build every operation without a script error. A manual script, `tests/http/docs_page_check.md`, covers what tests cannot see: the look, dark mode, phone width, the keyboard.

**Cost.** 101 KB more in an optimised executable that serves documentation, nothing in one that does not. A request for a file allocates about 1 KB, for its headers.

### D99. Security schemes and tags in the API documentation (OpenAPI plan, milestone 5)

Milestone 5 of the [OpenAPI plan](12-openapi-docs-plan.md), which finishes it.

**Schemes are values, routes name them.** `docs(security={"token": http.bearer()})` names the schemes; `security=["token"]` on a route says which it accepts. The names are strings on both sides because the annotation is checked at compile time and the `docs` call runs at run time, where the scheme values are built. So a route that names a scheme `docs` was not given cannot be a compile error; it panics when the document is first built, naming the route and the scheme, which happens on the first request to the docs routes or the first `openapi_json` call, so a test of the document finds it. Several names are alternatives (OpenAPI's list of requirement objects with one scheme each), which covers "a token or an API key"; combinations that must all be present are rare and left out.

**`SecurityScheme` is a struct with private fields and three constructors** (`bearer`, `basic_auth`, `api_key`) rather than an enum, so each constructor can have keyword arguments with defaults (`format`, `description`, `cookie`), and an invalid scheme cannot be built. Query-string API keys are not offered: keys in URLs end up in logs.

**Documentation, not enforcement.** The schemes describe what the service's middleware checks; the library does not check credentials itself, as `@fastify/swagger` and FastAPI's `openapi` models do not. The guide and docs/05 say so where the feature is introduced, and the test service enforces its schemes in middleware so that the test can check the document against the real answers.

**Tags.** `http.Tag(name, description)` values in `docs(tags=[...])` become the document's `tags`, in the order given, which the page follows for its groups (tags not listed come after, in order of first use).

### D100. Cookies, URL encoding, test requests with headers, and request locals (Google sign-in plan, milestone 1)

Milestone 1 of the [Google sign-in plan](13-google-oauth-plan.md): what `http` lacked for an OAuth flow, each useful on its own.

**Several values of one header are lines of one dict entry.** `Response.headers` stays a `dict[str, str]`; `add_header` joins a second value with a newline, and `serve` writes each line as its own header. This is how `set_cookie` sets any number of cookies on one response (the sign-in callback sets one and clears another), with no new field on `Response`, so responses that set no cookie cost what they did. Splitting on line breaks also means a value with a line break in it can no longer start a header of another name, which `with_header` used to allow. The client keeps repeated response headers the same way, where the last one used to win. A separate `cookies: list[str]` field was dropped: it would be allocated for every response.

**Cookie values are percent-encoded where they must be.** `set_cookie` sends a value as it is when every character is one a cookie may hold (RFC 6265's `cookie-octet`), so tokens, base64url and plain words are unchanged; anything else, and `%`, becomes `%XX`. `req.cookie` decodes `%XX` (a malformed escape is returned as sent). Express encodes every value; Go drops the characters it cannot send. An invalid name, path or `same_site` is a programming error and panics, which a handler turns into a 500 (D64). `__Host-` and `__Secure-` cookies, and `SameSite=None`, are always `Secure`, since browsers drop them otherwise.

**Encoding in the runtime.** `url_encode` keeps RFC 3986's unreserved characters and encodes every other byte, so its output is safe in a query string, a form body and a path segment; `url_decode` is the runtime's existing decoder, which the request parser already used. `form_encode` takes a list of pairs, which keeps the order and allows a name twice, and `post_form` sends one.

**Test requests.** `test_request(method, path, body="", headers={})` splits a query string off `path`, and `query`, `header`, `cookie` and `text` read the test's values. Before, a test request with a query string was routed with the query in its path, and a 404.

**Request locals.** `req.set_local(name, value)` and `req.local(name)` keep strings with the request, as the sign-in session needs; typed values would need a type parameter on `Request` or a dynamic type, and JSON text covers the cases so far.

**`Request` is no bigger.** A test request's parts and the locals live in one optional `extra` field, made only when a test or a middleware needs it. A first version added two fields, and the HTTP benchmark's counts caught it: 16 more bytes per request on `/plaintext` and `/json` (+6.7%), since the collector rounds objects up to 16 bytes. The single field took the place of the cached body text, `body_text: str?`, so `Request` shrank back to its old size and the counts are unchanged. The price is that `req.text()` on a real request reads the connection's buffer again on each call instead of returning a cached copy. Handlers read the body once (`http.body[T]`, `req.json()` and a `Json` parameter each call it once), and the docstring says to keep the result.

### D101. The `crypto` module (Google sign-in plan, milestone 2)

Milestone 2 of the [Google sign-in plan](13-google-oauth-plan.md): the primitives a signed session cookie, a PKCE challenge and an ID token's signature need, as a module of their own, since webhook signatures, CSRF tokens and signed links need them too.

**On `ring`, with no new crate.** The runtime already links `ring` for TLS (through `rustls`), so random bytes, SHA-256, HMAC and RSA verification cost no new dependency and no extra build time. Base64 is forty lines in the runtime rather than a crate.

**Data is `bytes`, text stays `str`.** Digests, keys and signatures are arbitrary bytes, so every function takes and returns `bytes`; base64 text and tokens are `str`. Passing a `str` where `bytes` is expected now has a help line, `text.encode()`, and the reverse names `hex()` and `decode()`, since the mix-up is the first error anyone hits with the module.

**One encoding per value.** The decoders accept padding or its absence, as JWT libraries do, but reject everything no encoder writes: a stray character, a wrong count of `=`, non-zero left-over bits, or a length of 4n+1. A signature or token that decodes must have been written exactly that way, which matters when a decoded value is compared.

**`equal`, not `==`, for secrets.** It folds the XOR of every byte pair and only then tests the result, through `black_box` so the optimiser keeps the loop, so its time depends on the lengths alone. A different length is `False` at once: lengths of MACs and digests are public.

**`verify_rs256` takes the key's numbers.** A JSON Web Key gives `n` and `e`, not a PEM file, so the function takes the decoded big-endian modulus and exponent (leading zero bytes are dropped, which `ring` would refuse) and accepts 2048- to 8192-bit keys. Its test uses a fixture signed by `openssl`, whose private key is not in the repository. Only RS256: Google signs ID tokens with it, and other algorithms can be added when something needs them.

**No encryption.** Decision 3 of the plan keeps no tokens, so the session cookie is signed, not sealed, and the AES-GCM functions the plan sketched are left out.

### D102. The `auth` module without signature checks (Google sign-in plan, milestone 3)

Milestone 3 of the [Google sign-in plan](13-google-oauth-plan.md): `stdlib/auth.lang`, written entirely in the language on `http` and `crypto`. It provides `auth.google(...)`, the login, callback and logout routes, the signed session and flow cookies, the policies, `on_login`, `required`, `protect`, `user` and the test helpers. The ID token's claims are checked, and its signature is left to milestone 4.

**The provider is frozen and holds the hook.** `Provider` is a `@frozen` struct of strings, tuples, keys and the optional `on_login` function. A function-typed field makes it unshareable by the usual rule, but nothing needs to share it: `install` registers closures over it. The standard library's own registrations are not checked for sharing, as `http`'s never were. Soundness comes from checking what the program gives the module: a standard library function's `on_...` arguments must be shareable where they are passed, and a function a standard library call makes from other functions (`auth.protect(handler)`) is shareable when the functions it was given are. Both reuse the existing E0338 message.

**The middleware tells the guard where to send browsers.** The session middleware stores the login route's path with every request, and the user's JSON when the cookie verifies (`auth.login` and `auth.user` locals). `auth.required` and `auth.protect` are then plain functions, with no provider to pass. A protected route on a server where no provider was installed finds no login path, and answers 500 with an error logged that says to call `install`. To redirect a browser back to the page it asked for, `http.Request` gained `query_string()`.

**Cookies.** The value is `base64url(json) + "." + base64url(HMAC-SHA256(key, "v1." + base64url(json)))`. The keys are HMACs of the secret with `"auth session v1"` and `"auth flow v1"`, so neither cookie can pass for the other. The names are `session` and `oauth`, or `__Host-session` and `__Host-oauth` for an `https` redirect URI. The flow cookie is deleted on every callback answer, success or not. The callback's refusals are short HTML pages, since only browsers reach it, and a refused ID token's reason is logged at warning level, without the token.

**Differences from the plan's sketch:**
- `install` takes a `Server`, not a `Group`; a group protects its routes with `group.use(auth.required)`.
- `logout_response` is a method of the provider, which knows the cookie's name.
- `test_cookie` takes `expires_in=` to make an ended sign-in for tests.
- `google_for_tests` takes the policies, `on_login`, the redirect URI and `endpoints=auth.Endpoints(...)` for a fake Google.
- The credentials file's `auth_uri` and `token_uri` are ignored: the endpoints are built in (decision 6), and the file's `auth_uri` is the older v1 one.
- An empty `SESSION_SECRET` counts as unset.
- A redirect URI may have a query; a fragment, a wildcard or a user name is refused, as Google refuses them.

**PKCE is sent though Google's web-server page does not describe it.** Read again on 2026-11-20, the page's token exchange lists no `code_verifier`, and the authorization response it shows has no `iss`. Google's discovery document advertises `S256` and the `iss` parameter. So the code sends PKCE as the plan says and checks `iss` only when present. If Google ever refused a verifier from a confidential client, the symptom would be `invalid_grant` on every sign-in, and the fix is to stop sending it. Milestone 6's check with a real client settles it.

### D103. ID token signatures and the key cache (Google sign-in plan, milestone 4)

Milestone 4 of the [Google sign-in plan](13-google-oauth-plan.md). The callback now checks the ID token's RS256 signature with `crypto.verify_rs256`, against the key its header's `kid` names in Google's JSON Web Key Set.

**The key cache.** A provider holds `Mutex[KeyCache]`: the keys by ID, frozen, and when they expire. A lookup takes the lock; the set is fetched when it has expired, or when it lacks the `kid`, since Google publishes a key before signing with it. A `kid` still missing after that fetch refuses the token. The lock is held across the fetch, which is safe because the runtime's `Mutex` parks the waiting tasks rather than blocking their threads, and it makes sign-ins that arrive together fetch once. The keys are kept for the response's `Cache-Control: max-age`, at least an hour and at most a day, or an hour without one. Only keys with `kty` RSA, `alg` RS256 (or none) and `use` sig (or none) are kept.

**Fetching again for each unknown `kid` is not rate-limited.** The ID token comes from Google's token endpoint over TLS, in answer to a code only Google issued, so nobody else chooses its `kid`. A cool-down would also delay the first sign-in after Google rotates its keys.

**Fakes that cannot sign.** `auth.Endpoints(keys="")` skips the signature check, and its docstring says it is for fakes. Only `google_for_tests` takes endpoints, so a provider made by `auth.google` always checks. The signing tests are in Rust (`crates/lang_cli/tests/oidc.rs`, with `ring`, already in `Cargo.lock`, as a dev-dependency), since `crypto` has no RSA signing and the plan rejected adding it for tests alone.

### D104. Routes protected by the compiler (Google sign-in plan, milestone 5)

Milestone 5 of the [Google sign-in plan](13-google-oauth-plan.md). When a program imports `auth`, `@auth.required` on an `@http` route, or a parameter of type `auth.User`, protects the route: `server.mount`'s generated adapter starts with `match __auth.guard(__req)`, which returns the refusal (a 401, or a redirect for a browser) before any parameter is read. An `auth.User` parameter is bound by `auth.user_param(req)?` after the guard, and an `auth.User?` one by `auth.user(req)`, without a guard.

**The user binding comes before the JSON body.** `auth.User` derives `Json`, so without its own rule it would be read from the request body. The compiler checks for it right after `http.Request`.

**One new code.** The misuses (`@auth.required` off a route or with arguments, `auth.User` under `@http.ws`, two `auth.User` parameters) are E0342, a code of their own rather than E0340, since they are about signing in, not the route's shape. Using the annotation without importing `auth` is an unknown annotation (E0330) whose help says to import it; the routes are then not protected, since the program does not compile.

**`@auth.required` is allowed on WebSocket routes.** The guard runs on the upgrade request like on any other. An `auth.User` parameter there is still an error, as the plan says, though the adapter could hand the user to the connection's task; that is left for when WebSocket handlers get other bindings.

**The documentation names the real cookie.** `install` registers the `googleSession` scheme with `server.security_scheme`, a new public method for libraries, rather than the compiler writing a fixed cookie name, which would be wrong for `https` (`__Host-session`). The compiler adds `googleSession` to each protected route's `security` and a `401` to its responses. `docs(security=...)` overrides a scheme of the same name.

### D105. Binary bodies, client addresses, request IDs and structured logs (production web plan, milestone 1)

Milestone 1 of the [production web plan](14-production-web-plan.md).

**`http.binary`, not `http.bytes`.** A function named `bytes` in the `http` module hides the prelude type of the same name inside the module, so the response constructor is `http.binary(data, content_type, status)`. docs/05 had listed `http.bytes` and `req.body() -> bytes` since the first design without either being built; both now exist under these names. The response keeps the data in a private `bytes?` field and `serve` passes it to the same `lrt_http_respond`, which never treated the body as text, so there is no second entry point.

**The request ID lives with the task, counted.** Each fiber's record holds an optional reference-counted string (two words, `None` for a task with no request), and a new fiber takes a reference to its parent's, so tasks a handler spawns log with its ID. It is counted rather than collected because code outside the fiber runtime keeps it in a thread-local, which the collector does not scan. A first version kept the ID inline (129 bytes), which made every task bigger whether or not it served a request. A handler that panics leaves it set; `serve_connection` clears it then, so the connection's next request cannot log with a stale ID. A request that panics gets no access line, since the middleware never resumes; the panic report is the record.

**Fields are NUL-separated text to the runtime.** `log` passes the fields as `name\0value\0...`, and the runtime formats both forms, so the standard library needs no JSON or quoting code and `log.info(message)` without fields costs what it did. NUL characters in a field are dropped. `lrt_log_enabled` lets a disabled level skip building the fields.

**Buffering only while a server runs, through one lock.** A per-thread buffer would reorder a task's lines when the task moves between worker threads, so lines go to one buffer under a mutex, flushed at 16 KiB, at once for `warn` and above, on each wake of the accept loop (at most 200 ms apart, which needs no timer), and by `out::flush`, which the exit, panic and `io.eprint` paths already call. Outside `serve`, or when standard error is a terminal, each line is written as before. The plan said 100 ms through the timer thread; the accept loop's 200 ms needs no new machinery.

**Request IDs without a system call.** Until `uuid` exists (milestone 6), an ID is a process counter put through SplitMix64's finaliser, keyed by 8 random bytes read once: 16 hex digits, distinct within the process and unpredictable across processes. The plan suggested a random token, which would cost a `getrandom` per request.

**`client_ip` and `trusted_proxies`.** `X-Forwarded-For` is believed only from a peer in `server.trusted_proxies`, walked from the right past trusted hops, so a client's own entries never win. The list is process-wide, set by `bind` like `max_body`, because a request holds no reference to its server. An entry that is not an address ends the walk at the hop before it. `LOG_FORMAT`, like `LOG`, takes the language's prefix, which the plan left out.

**Not done here:** the access line has no route pattern or response size; both come with the metrics milestone, where the runtime knows the matched route and writes the body. `os.setenv` is unsafe in the presence of other threads reading the environment, as in every C runtime; its docstring says to call it at startup.

**Counts.** `Response`'s `data` field moves every response into the next allocation size class, 16 bytes more per request on every benchmark endpoint, and even a one-word field does. The owner approved re-recording `bench/http/counts.json` for it; it is the one exception so far to docs/14's rule that unused features leave the counts unchanged.

### D106. CORS (production web plan, milestone 2)

Milestone 2 of the [production web plan](14-production-web-plan.md): `http.cors(...)`.

**`allow` beside `origins`.** The plan let `origins` be a list or a function; the language has no union types, so the function is a separate `allow: (def(str) -> bool)?` argument, consulted for origins the list does not name. Passing a lambda there needed the checker to see through the optional: a lambda where `F?` is expected now takes its parameter types from `F`.

**Preflights through a group.** Middleware on a group wraps each of its route handlers, so it never sees a request the router refuses, and the router refuses every preflight (no route accepts `OPTIONS`). A route registered by a group with middleware now also keeps that middleware wrapped around a 405; when no route accepts an `OPTIONS` request that carries `Access-Control-Request-Method`, the router hands it to the route that accepts the method asked about, if it has one. The group's `cors` answers it; a group without `cors` sends the same 405 through its middleware. Routes never pay for this: it runs only where the router would have answered 405.

**Headers on error responses.** A handler's `Err` becomes a response in `Server.handle`, after the middleware has returned. Middleware now records headers for that response on the request (`RequestExtra.error_headers`, allocated only when used), which `request_log`'s `X-Request-Id` also uses instead of its own special case. So a browser can read a 404's body from another origin.

**Smaller choices.** `*` is sent only for `origins=["*"]` without credentials, and then without `Vary: Origin`; a named origin is echoed with `Vary: Origin` (added to any `Vary` the handler set). A preflight from a refused origin gets a 204 with only `Vary`, never reaching a route. An `OPTIONS` request without `Access-Control-Request-Method` is not a preflight and is routed as before. Setup panics for no origins, for an origin with a path (a trailing `/` is the usual mistake, and browsers never send one), and for credentials with `*`.

### D107. Static files (production web plan, milestone 3)

Milestone 3 of the [production web plan](14-production-web-plan.md): `server.static(...)`.

**The runtime sends the file; the language sees a plan.** `server.static` registers one `GET` route, `prefix/{path...}`, whose handler passes the request's headers to `lrt_http_static`. The runtime checks the path, opens and `fstat`s the file, evaluates the conditions and returns the status, content type and headers as text; the file itself (descriptor, offset, length) stays on the Rust `Request`. The handler builds an ordinary `Response` with an empty body, so middleware sees and may change it. When the response comes back to `lrt_http_respond` with the same status and still no body, the runtime writes the head with the file's length and then the file: files up to 1 MiB are read into the connection's buffer; larger ones are sent after the handler returns, in 256 KiB pieces read with `pread` into one Rust buffer, so no file passes through the collected heap and a 20 MiB download leaves resident memory where it was. A middleware that replaces the body, or the status, simply wins. Test requests (no socket) get the bytes in the response's `data` instead, which also means a test `HEAD` returns the body that a real one omits.

**The path is checked raw.** The router decodes the path before matching, which would hide an encoded `/` or `\`. The runtime keeps the raw path beside the decoded one and the file server works from it, dropping the prefix's segments: it percent-decodes each segment once, refuses decoded separators, `..`, NUL, backslashes, empty middle segments, dotfiles (unless allowed), segments over 255 bytes and paths over 4096, collapses `.`, and then requires the file's canonical path to start with the root's, which catches symbolic links leading out. Every refusal is a 404, like a missing file.

**A strong `ETag`.** The plan said `W/"size-mtime"`. `If-Range` and `If-Match` use strong comparison, so a weak tag would make resumed downloads restart from the beginning; and size plus modification time to the nanosecond identifies the bytes as well as any tag that is not a hash. So the tag is strong, and a pre-compressed variant's tag carries its encoding. `If-None-Match` still compares weakly, as RFC 9110 says, so `W/` forms sent back by proxies match.

**Smaller choices.** Conditions are evaluated in RFC 9110 section 13.2.2's order (`If-Match`, `If-Unmodified-Since`, `If-None-Match`, `If-Modified-Since`, then `Range` with `If-Range`). Only the IMF-fixdate form of HTTP dates is read; the two obsolete forms count as no date, which makes the request unconditional rather than wrong. `max_age` defaults to 0, so browsers revalidate (a 304 costs one round trip) rather than serve stale files after a deploy; hashed names take `max_age=31536000, immutable=True`. Pre-compressed files are skipped for range requests, since a range of the compressed bytes is rarely what a player asked for. The fallback file is sent with `Cache-Control: no-cache`, because it stands for every path. Static routes are left out of the API documentation, as the documentation's own routes are. File reads block the worker thread, as `io.read_file` does; for local disks they are short.

**Cost.** The Rust `Request` gained two optional fields (the file plan and a pending stream), which are not collected memory; servers without static files allocate the same as before.

### D108. Compression (production web plan, milestone 4)

Milestone 4 of the [production web plan](14-production-web-plan.md): `http.compress(...)`, and gzip in the HTTP client.

**The encoder.** `miniz_oxide` 0.9 (owner decision 1 in the plan: pure Rust, no C, MIT, Apache-2.0 or Zlib), with only its allocation feature, for raw deflate both ways. The gzip framing (RFC 1952: a ten-byte header, the CRC-32 and the length) is forty lines in `lang_rt/src/gzip.rs`, with the CRC table built at compile time; `flate2` would have added a crate for that alone. The decoder reads several members in a row, as `cat a.gz b.gz` makes, and checks each trailer.

**Middleware, not the runtime.** Compression is a policy (which types, what size, which responses), and middleware already sees the finished response, so `http.compress` is ordinary middleware that replaces the body with gzip bytes through `lrt_gzip`. It costs nothing when unused, and the per-request counts are unchanged. Errors returned as `Err` become responses after the middleware has returned, so they are not compressed; they are short. Files from `server.static` reach the middleware with an empty body (the runtime sends the file afterwards, D107), so they are left alone, and their pre-compressed `.gz` files do the job.

**Choosing.** `Accept-Encoding` is read in the runtime, sharing the static file server's q-value parser: gzip is chosen when it, or `*`, has a q-value above 0. A client that also refuses the body as it is (`identity;q=0`, or `*;q=0` without `identity`) gets gzip even below `min_size` or for other types, rather than a 406. `Vary: Accept-Encoding` is added to every response that could be compressed, whether or not this client asked, so a shared cache never gives a compressed body to a client that cannot read it. A strong `ETag` becomes weak, since the bytes differ from the uncompressed ones. `Cache-Control: no-transform` is honoured.

**The client.** `http.get`, `post` and `fetch` send `Accept-Encoding: gzip` and decode a gzip body, removing `Content-Encoding` and `Content-Length` from the headers, as Go's client does; a caller that sets `Accept-Encoding` itself gets the body as sent. A decoded body is limited to 1 GiB, so a small body cannot expand without bound.

### D109. The language is named Viper, with the `.vi` extension

The working name was Weby, with `.wb` files. At the owner's request the language is now **Viper**: the command is `viper`, source files end in `.vi`, environment variables start with `VIPER_`, and projects use `viper.toml`, `viper.lock` and `.viper/deps`.

The rename followed the checklist in [03](03-compiler-architecture.md#renaming-the-language): the constants in `lang_names`, the binary name, every source file moved with `git mv` (so history follows), and the docs, benchmarks, examples, test goldens and editor support (the tree-sitter grammar, whose generated parser was renamed in place, and the VS Code, Vim and Zed files). No Rust source outside `lang_names` changed, which is what that crate is for.

The old extension is not accepted alongside the new one: the repository is private, so no one outside depends on `.wb` files. Two collisions were noted and accepted: `.vi` is also LabVIEW's extension, so tools that guess a language from the extension (GitHub's language statistics among them) may call these files LabVIEW; and Viper was the original name of the Ethereum contract language now called Vyper.

### D110. Dates and times (production web plan, milestone 5)

The `datetime` module of docs/14, section 4.5, with these choices where the plan left room or the language pushed back:

- **The representation.** A `DateTime` is whole seconds since the epoch, nanoseconds (0 to 999,999,999), the offset it is shown with, and its zone (0 for a fixed offset, otherwise the runtime's id for a loaded zone). The plan said integer nanoseconds; that covers only 1678 to 2262, and the module promises years 1 to 9999 and instants before 1970, so the seconds and the fraction are kept apart. A `Duration` is integer nanoseconds as planned (about 292 years either way, as in Go). A `Date` is a day number. All four types are `@frozen` with private fields, so values are made only through the module's functions and can be shared.
- **Equality and order are by instant.** The same moment shown in London and in New York is `==`, hashes the same and sorts together, as in Go's `Equal` and Python's aware datetimes. The zone is part of how a value prints, not what it is.
- **Methods, not operators, between instants and durations.** The arithmetic traits are `Self` with `Self` (`AddNumber.add(self, other: Self)`), and the trait system has no operand type parameter, so `t + d` cannot be expressed. The plan's `t + datetime.hours(2)` and `t - other` are `t.add(d)`, `t.sub(d)` and `t.since(other)`; durations do have `+`, `-` and `<`. The compiler's `cannot use +` error now names the method when the left side has an `add` (or `sub`) method taking the right side. A generic operand type parameter would make all the number traits harder to read for one module's sake.
- **`time.sleep` keeps its float.** There is no overloading and a trait cannot be implemented for `float` outside the prelude, so `time.sleep` cannot take both. `datetime.sleep(d)` takes a `Duration`; `time.sleep(seconds)` is unchanged. The HTTP client gained `timeout: datetime.Duration` on `http.fetch`, defaulting to 30 seconds as before.
- **Constructors take integers.** `seconds(n)`, `millis(n)` and the rest take `int`, so a duration is exact; a fraction of a second is `millis(1500)` or `parse_duration("1.5s")`.
- **Wall-clock times.** In a zone, a time that happens twice is the earlier instant, and a time in a gap moves forward by the gap, as Go and Python's `fold=0` do. The runtime finds the offsets in force a day either side and keeps the ones that map back to the same wall time, which is right unless a zone changes its offset twice within two days.
- **Time zones are runtime code.** Reading TZif files (versions 1 to 3, with the POSIX TZ footer for instants past the table, including quoted names and `Jn`, `n` and `Mm.w.d` rules) is Rust in `lang_rt/src/datetime.rs`, as are parsing and formatting: they work on bytes and run on every JSON value with a date in it. The calendar, the types and their methods are Viper. Zones are cached by name for the life of the process. `TZDIR` overrides `/usr/share/zoneinfo`, which is how the tests use the fixture files in `tests/fixtures/zoneinfo`; Windows gets UTC and fixed offsets unless `TZDIR` names a copy of the database (section 5, decision 2 of the plan).
- **JSON through a trait.** The prelude gained `JsonString` (`to_json_string` and a static `from_json_string(text) -> Result[Self, str]`). Implementing it implements `ToJson` and `FromJson`; the generated encoder writes the method's text as a JSON string, and the decoder reads a string and passes it to the method, turning its error into a decode error at that path. Any user type can use it, for example money written as `"12.34"`. OpenAPI shows such types as strings, with `date-time` and `date` formats for the two `datetime` types it knows.
- **PostgreSQL.** `query_as` reads `timestamptz` text as `parse` accepts it (`2026-11-20 09:30:00+00`); a `timestamp` (no zone) gets a `Z` appended in the runtime, so it reads as UTC; a `date` is already `YYYY-MM-DD`.
- **Parse errors are 400s.** `DateTimeError.status()` is 400, since the text usually comes from a request; a zone name that fails to load at startup is a programming error the caller sees before serving.
- **Two compiler fixes found on the way.** A parameter default that constructs a struct with field defaults (`zone: Zone = Zone(0)`) was checked before field defaults were, and reported the defaulted field as missing; field defaults are now checked on first use. A module-qualified type as an explicit type argument (`json.decode[datetime.Date](text)`, `json.decode[list[json.Value]](text)`) was rejected as "not a type"; it now resolves like a type annotation.

### D111. UUIDs (production web plan, milestone 6)

The `uuid` module of docs/14, section 4.5, and request IDs made with it:

- **The representation is the text.** A `Uuid` keeps its 36-character lowercase form in a private field. Equality, hashing and order are then the derived ones on a string, and lowercase hex sorts as the bytes do, so version 7 UUIDs sort by time without a hand-written `Ord`. Two `int` fields would be smaller, but the language's integers are signed, so their derived order would put UUIDs with the top bit set first, and every print and JSON write would format them again. `bytes()` parses the text when asked.
- **The v7 counter.** RFC 9562's method 1 with a 42-bit counter (the 12 bits of `rand_a` and the top 30 of `rand_b`), started at a random value below 2^41 in each new millisecond, so at least 2^41 UUIDs fit in one millisecond; the last 32 bits are random in each UUID. If the counter does run out, or the clock goes back, the timestamp moves on from the last one used, so each UUID is greater than every one before it in the process. One mutex guards the last timestamp and counter; it is held for a comparison and two stores.
- **The clock is `datetime`'s.** `v7` reads the instant `datetime.now()` does, so `datetime.set_clock_for_tests` fixes it too. That is how the test makes 100,000 UUIDs in one millisecond.
- **Random bytes are pooled.** Each thread reads 4 KiB from the operating system's secure source at a time and hands out bytes it then zeroes, so `v4` and `v7` cost a system call once in about 400 calls. `http.request_log` makes one per request; D105's request IDs avoided the system call with a scrambled counter, and this keeps that property while making the IDs standard and sortable. The old generator is gone.
- **Parameters with a string form.** The plan asked for `uuid.Uuid` path parameters. Rather than a special case, any struct or enum implementing `JsonString` (D110) is now a path or query parameter, read through `from_json_string`, with its error message in the 400: `uuid.Uuid`, `datetime.Date`, `datetime.DateTime` and users' own types. The generated adapter reaches `from_json_string` through the JSON decoder (the text is quoted and decoded), because a type parameter's static methods cannot be called directly; the cost is one string copy per parameter. A query parameter of such a type may default only to `None`, since a literal of another type cannot be checked against the type's parser at compile time. Such a type is therefore no longer taken as a JSON body when it is not in the path, which no program did: a body is an object or a list.
- **Parsing is strict.** Only the hyphenated 36-character form, in either case; not braces, `urn:uuid:` or 32 digits without hyphens. An ID in a URL or JSON is written one way, and accepting others would let two spellings of one key into logs and caches.

### D112. Configuration (production web plan, milestone 7)

The `config` module of docs/14, section 4.6:

- **Settings are decoded by the JSON code (decision 8's default).** `load[T]` turns the variables into a JSON object of strings, keyed by the lower-cased name without the prefix, and decodes it with `T`'s derived `FromJson` code. A `@derive(Config)` would have given per-field variable names, but it would be a second derive generating nearly the same code, and every type with a string form (`Duration`, `Uuid`, users' `JsonString` types) works here for free.
- **The lenient mode is a reader flag.** `load` sets a thread-local just before `json.decode`, and the reader takes it (and clears it) when it starts, so no other decode on the thread is lenient and the generated code needs no new parameter. In the mode, a string where a number or boolean is expected is parsed from its trimmed text (`yes`/`no`/`1`/`0` for booleans), and a string where an array or object is expected is spliced into the input as JSON when it is JSON, or, for an array, split on commas into strings. Spliced buffers are kept until the reader is freed, since decoded keys may point into them. Strict decoding is unchanged apart from one branch on the flag per number, boolean, array and object read.
- **Every problem in one error.** In the mode, when a top-level field fails, the generated code asks the runtime to recover: the error is kept as a problem, the field's value is skipped from where it began, the field gets its type's zero value, and decoding goes on. A missing required field is a problem too. The decode then fails with all of them, so the partly zeroed struct is never seen. Recovery only at the top level keeps the generated code small (one call per field) and the values well formed: a nested value that fails is one problem for its variable, with the path inside it.
- **Messages name variables.** A problem's path `port` becomes `PORT` (with the prefix), the rest of the path follows as `at ...`, and the problems are sorted, so the error is the same whatever order the fields are declared or the environment is listed in.
- **`Secret` hides itself everywhere it can.** `str()`, f-strings, a derived `Display` and JSON give `***`, so neither `log.info(f"{settings}")` nor returning the settings from a debug endpoint leaks it. Encoding then does not round-trip, which is the point: a secret should not travel as JSON. `reveal()` is the one way out, easy to search for. It is `@frozen` with a private field, so it is made by decoding or by `config.secret(value)`.
- **The `.env` parser is in the runtime**, written to a grammar documented in docs/05 and in the parser's own comment, with unit tests for each rule. A malformed line stops the whole file with its line number: a skipped line would leave a variable silently unset in production. `export` is accepted so that the same file can be sourced by a shell, and the rules follow what `docker compose` and the common `dotenv` libraries agree on, without their interpolation.
- **The environment is read once, as a whole.** `os.environ()` (new) lists it, and `load` overlays it on the file's variables, so the real environment wins (decision 7). `load` does not change the environment; `load_env` does, and only for variables not already set unless `override`.

### D113. Metrics, health and readiness (production web plan, milestone 8)

The metrics and health checks of docs/14, section 4.7, without the dashboard (milestone 9):

- **Off costs nothing.** `server.metrics()` sets one flag in the runtime. Until then the request loop checks it twice per request (two relaxed loads) and reads no clock. The one cost that remains on is `dispatch` telling the runtime which route took the request (`lrt_http_set_route`, one store), which `req.route()` and the access log's new `route` field use as well.
- **Route labels are slots.** Each route gets a slot when it is added (`new_route`, not `compose`), interned by method and pattern, so the same `GET /x` in two servers or groups shares one; its method and pattern are leaked once, so labels and `req.route()` cost no copy. Slots past 1024 share `other`; requests no route took are `unmatched`. The plan's limit on series follows: routes times the methods that reach them times the statuses seen.
- **Recording takes no lock and allocates nothing.** Sixteen shards, one chosen per thread on its first request: per route, a cache-line-aligned block of 17 bucket counts, the nanoseconds and the bytes; and a 4096-entry open-addressing table of `(route, method, status)` counts whose keys are claimed with a compare-and-swap. The tables are allocated zeroed when recording starts, about 2.2 MB of address space, so routes that see no request never touch their pages. A scrape adds the shards.
- **What is timed** is from the request having been parsed to the handler having returned, with `Instant` (one `clock_gettime` each side). Writing the response to the socket is not included: it happens later, batched with other responses on the connection. A panic is a 500 and an `errors_total{kind="panic"}`; read errors are counted by kind without a route.
- **The histogram's `method` is the route's own.** A route serves one method (and `HEAD` for `GET`), so labelling its histogram by the request's method would only split `GET` from `HEAD`; `http_requests_total` keeps the request's. `unmatched` has no method label on its histogram for the same reason.
- **Application metrics are a registry of their own**, a `RwLock<Vec<Metric>>` with a `RwLock<HashMap>` of series per metric, values as `f64` bits in `AtomicU64`s changed by compare-and-swap loops. An update with seen labels takes two read locks; a new set of values takes the metric's write lock. Handles are `@frozen` structs holding the id, name and label names, so they can be shared and checked in the language before the runtime sees them. The 1000-set cap folds the rest into `other` rather than refusing, so a mistake costs accuracy, not requests.
- **Credentials are digests.** The user name and password are kept as HMAC-SHA256 under a random per-process key, and both are compared, each in constant time, whatever the first gave. Comparing digests makes the time independent of the guess's length too. `METRICS_PASSWORD_HASH` waits for milestone 9, which settles its format with `viper hash-password`.
- **Readiness runs the checks in tasks.** A check is a function value, which the sharing rules do not let a program spawn; the standard library is allowed to spawn the function values it was given (`spawn` in a standard library module skips the check for function-typed callees and arguments), and `server.health(checks={...})` is checked where it is called: each function in a dict literal must be shareable, and a dict that is not written out is refused, since its functions cannot be checked. The handler waits for all of them against one two-second deadline.
- **SIGTERM drains; SIGINT does not.** SIGTERM and `server.stop()` set a stop request; the accept loop, which wakes every 200 ms, turns it into the drain (readiness fails, everything else is served, new connections accepted) and stops after `drain_delay` seconds, then waits for open connections as before. SIGINT, from a person at a terminal, stops at once. With the default delay of 0 nothing changes.
- **The format.** Families are written whole, `# HELP` then `# TYPE` then their samples, the HTTP ones in a fixed order, series sorted; numbers in Rust's shortest form, `+Inf` for the last bucket. A family with no samples yet still has its two lines. `metrics.snapshot_for_tests()` reads the same text, so tests see what Prometheus would.
- **What it costs, measured.** Metrics alone add two clock reads per request and nothing else, and the rate at 256 connections stays within 1.3% of plain. With the access log as well (`counts.py --observed`), a request costs four more clock reads and about 30 allocations, and the rate drops 31% to 42%: the access log, not the metrics, is over the plan's budget, and making it cheaper is left as a follow-up.

### D114. The metrics dashboard (production web plan, milestone 9)

The dashboard of docs/14, section 4.8, with `METRICS_PASSWORD_HASH`, the lock-out and `viper hash-password`:

- **Embedded, and only when used.** The page, stylesheet and script are `include_str!`'d into `lang_rt` and reached only through `lrt_dashboard_asset`, which only `server.metrics` calls, so `--gc-sections` leaves them out of other programs, as with the documentation viewer; a test looks for a line of the page in both kinds of binary. The page is a template with one placeholder, the dashboard's path, filled in once. The stylesheet and script reuse the viewer's `DocsFile` (ETag, `no-cache`); the page and the data are `no-store`.
- **No chart library.** The script draws SVG itself (stacked areas, lines, a crosshair tooltip, sparklines) in about 17 KB. The colours are a validated categorical set, one for light and one for dark mode, defined as CSS custom properties; status classes have their own fixed colours (4xx amber, 5xx red), so a filter never repaints a series. The strict CSP rules out inline styles, so positions are set through the CSSOM, which CSP allows.
- **History is deltas.** A thread named `metrics-history` wakes every 5 s, adds up the shards (as a scrape does) and stores what changed since the last sample: requests by status class, the 17 duration buckets, each busy route's requests, and the gauges. Percentiles are worked out when `data.json` is read, from the summed buckets of the samples asked for, by linear interpolation within a bucket, like `histogram_quantile`. 720 samples make an hour; a sample with 20 busy routes is about 600 bytes.
- **Events cost one comparison.** `record()` checks a flag and whether the status is 500 or more or the duration above the 20th-slowest of the last five minutes (an atomic floor, 0 until there are 20); only then does it take a lock. The request ID has been cleared by then, so it is read back from the response's `X-Request-Id`, which `request_log` set. The path is the request's path, which never includes the query.
- **The hash is PBKDF2.** `pbkdf2-sha256$<iterations>$<salt>$<hash>` with standard base64, checked with `ring::pbkdf2::verify` outside the credentials lock. After one success the password's HMAC digest is kept, so later requests compare digests in constant time as with a plain password; a wrong password always pays the full PBKDF2, which also slows guessing.
- **The lock-out counts only wrong credentials.** A browser asks first without an `Authorization` header and then shows its login box, so requests without one do not count. Ten failures within a minute from one `client_ip` lock that address out for a minute: a 429 with `Retry-After`, even with the right password, so guessing cannot continue by luck. The table is capped at 4096 addresses: when it is full, addresses with no failure in the last minute are forgotten, then any that is not locked out; Milestone 14 moves this onto the rate limiter. Test requests have no address and share one entry.
- **`hash-password` is a compiler command**, so the hash is made where the program is built, with no network. It reads the password from the terminal with `stty -echo` (twice, to catch typos), or one line from standard input in scripts.
- **Data is pulled.** The page polls `data.json?since=<last sample>` at the sampling interval and keeps up to 720 samples itself, so a poll carries one new sample plus the totals, routes and events. Milestone 15 adds a live mode over Server-Sent Events.

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
