# 03 — Compiler architecture

The compiler is a Rust program, `weby`, that turns a project's `.wb` files into one native executable. It is a batch, whole-program compiler: it reads every module reachable from the entry file, checks them, generates one LLVM module, and links it with the runtime.

## Repository layout

A Cargo workspace. Few crates at first; split further only when compile times or boundaries demand it.

| Path | Kind | Contents |
|---|---|---|
| `crates/lang_names` | library | The language name, CLI name, file extension and environment prefix as constants (see below) |
| `crates/lang_diag` | library | Source map, spans, diagnostic type, rendering, error codes |
| `crates/lang_format` | library | Python's format-spec mini-language and float formatting, shared by the compiler (to check f-string specs) and the runtime (to apply them) |
| `crates/lang_syntax` | library | Tokens, lexer, AST, parser |
| `crates/lang_sema` | library | Module graph, name resolution, type interner, type checker, trait tables, HIR, derivations, monomorphisation |
| `crates/lang_codegen` | library | HIR to LLVM IR through `inkwell`, object emission |
| `crates/lang_cli` | binary `weby` | Argument parsing, pipeline driver, linker invocation |
| `crates/lang_rt` | static library | The runtime ([04](04-runtime-and-memory.md)) |
| `stdlib/` | Weby source | Standard library modules ([05](05-standard-library.md)) |
| `tests/` | data | End-to-end test programs and expected output ([07](07-testing-and-performance.md)) |
| `bench/` | data | Benchmark programs in Weby, Go and Python |
| `docs/` | markdown | These documents, later the language reference |

Spans and the source map live in `lang_diag` rather than `lang_syntax`, so that every phase, the lexer included, can report diagnostics without a dependency cycle.

`lang_sema` and `lang_syntax` must not depend on LLVM. That keeps `weby check` fast, keeps the frontend testable without LLVM installed, and lets a future language server reuse them.

### Renaming the language

The name "Weby" and the `.wb` extension may still change, so the name is kept out of everything that is expensive to rename.

- **Crates** use the neutral prefix `lang_`. **Runtime symbols** use the neutral prefix `lrt_`. The runtime archive is `liblang_rt.a`. None of these change on a rename.
- One small crate, `crates/lang_names`, holds every name-dependent constant: the language's display name, the CLI name, the source file extension, the environment variable prefix (`WEBY_`), and the cache directory name. The compiler, the runtime and the test harness read these constants; none of them contains the name as a literal.
- A CI check searches the Rust sources outside `lang_names` for the current name and extension and fails if it finds either.

Rename checklist:

1. Change the constants in `lang_names`.
2. Change the binary name in the `lang_cli` manifest, and `CLI` at the top of the Makefile.
3. Rename source files (`stdlib/`, `tests/`, `bench/`) to the new extension with a one-line script; the test harness finds files through the constant.
4. Search and replace in `docs/` and the README.
5. Update the editor syntax-highlighting file association, once one exists.

After the first public release a rename also means keeping the old extension working for a while, which the import resolver can do by accepting a list of extensions.

### External crates

| Purpose | Crate | Notes |
|---|---|---|
| LLVM bindings | `inkwell` | Feature flag selects LLVM 19 |
| Diagnostics rendering | Hand-written, in `lang_diag` | Chosen over `ariadne` and `codespan-reporting` at M0: plain text with no colours keeps golden tests stable, and it is about 100 lines |
| CLI parsing | `clap` | |
| Snapshot tests | `insta`, if needed | Diagnostics goldens are plain `.stderr` files under `tests/ui`, regenerated with `WEBY_BLESS=1`; lexer and parser tests are ordinary assertions so far |
| String interning | `lasso` or a small hand-written interner | |

## Pipeline

| # | Phase | Input | Output | Fails with |
|---|---|---|---|---|
| 1 | Load | Entry path | Source map of files | I/O errors |
| 2 | Lex | Source text | Token stream with layout tokens | Bad indentation, bad literals |
| 3 | Parse | Tokens | AST per file | Syntax errors (recovers and continues) |
| 4 | Module graph | ASTs, imports | Ordered module list | Missing module, import cycle |
| 5 | Resolve | ASTs | Symbol tables; names bound to definitions | Unknown or duplicate names |
| 6 | Type check | Resolved ASTs | Typed HIR | Type errors, trait errors, non-exhaustive match |
| 7 | Monomorphise | HIR, starting at `main` | Closed set of concrete functions and types | (none; bodies were already checked) |
| 8 | Codegen | Concrete HIR | LLVM module | Internal errors only |
| 9 | Optimise and emit | LLVM module | Object file | Internal errors only |
| 10 | Link | Object, `liblang_rt.a`, GC library | Executable | Linker errors |

Phases 2 to 6 collect as many diagnostics as they can before stopping. If any error was reported, the driver stops before phase 7. Phases 7 onward assume a correct program, so a failure there is a compiler bug and is reported as such.

## Lexer

Hand-written, operating on bytes, producing tokens with byte-offset spans.

### Layout algorithm

The lexer converts indentation into three synthetic tokens so the parser can be an ordinary context-free parser.

- State: a stack of indentation widths, starting with `[0]`, and a bracket depth counter.
- At the start of each logical line, measure the leading spaces.
  - Blank lines and comment-only lines are skipped and produce nothing.
  - Wider than the top of the stack: push the width, emit `INDENT`.
  - Equal: nothing.
  - Narrower: pop and emit one `DEDENT` per popped level until the top equals the width. If the width matches no level on the stack, report "unindent does not match any outer indentation level" and recover by treating it as the nearest level.
- At the end of a line, emit `NEWLINE` if bracket depth is zero.
- Inside brackets (depth above zero), no `NEWLINE`, `INDENT` or `DEDENT` is emitted.
- A tab in leading whitespace is an error with a fix-it.
- At end of file, emit a final `NEWLINE` if needed, then one `DEDENT` per remaining level.

### Other lexer details

- Numbers: decimal, `0x`, `0o`, `0b`, underscores as separators, floats with exponent.
- Strings: single, double and triple quoted; standard escapes; `\u{...}`.
- f-strings (M2): the lexer emits `FSTRING_START`, then alternating literal chunks and ordinary token runs for each `{expression}`, then `FSTRING_END`. It keeps a small mode stack to handle nested brackets and nested strings inside expressions. Format specs after `:` are passed through as a literal chunk.
- Keywords are recognised by lookup after scanning an identifier.

## Parser

Hand-written recursive descent for declarations and statements; precedence climbing (Pratt) for expressions, using the table in [01](01-language-design.md#grammar-sketch).

- **AST storage.** Nodes live in per-file arenas and are referred to by typed indices. Every node carries a span. The AST keeps enough detail (parentheses, comments attached to declarations) for a formatter to be written later.
- **Error recovery.** On an unexpected token, report once, then skip to the next `NEWLINE` at the current block depth, or to the matching `DEDENT`. Because blocks are delimited by layout tokens, recovery is more reliable than in brace languages: one bad statement rarely damages the rest of the file.
- **Generic call ambiguity.** `name[A, B](args)` could be an index followed by a call, or a generic instantiation followed by a call. The parser always produces index-then-call. The type checker reinterprets the index as type arguments when the base resolves to a generic function or type. Type positions (after `:` and `->`) are parsed with the type grammar and are unambiguous.
- **Lambda and ternary** bind loosest and are parsed before entering the Pratt loop.
- **`match` and `case`** are reserved words (unlike Python's soft keywords), which removes a class of lookahead problems.

## Module graph and name resolution

- Starting from the entry file, follow `import` statements. `import a.b` maps to `a/b.wb` under the project root, then under the standard library root.
- The prelude (built-in types, traits and functions such as `print`, `len`, `range`) is implicitly imported into every module.
- Resolution is two-pass per module: first declare every top-level name, then resolve references inside signatures and bodies. Declaration order within a file never matters.
- Inside function bodies, a per-function table records each local at its first assignment.
- Visibility is enforced here: a reference to a declaration in another module must find it marked `pub`, and a reference to a member marked `priv` must come from the file that defines its type.

Before M5 there is exactly one user module, plus standard library modules that the compiler finds through the same mechanism. That means the import machinery for the standard library is built early, and user-file imports reuse it.

## Type checking and HIR

Described from the language side in [02](02-type-system-and-traits.md#type-checker-implementation-notes). The output is HIR: a tree, fully typed, with all sugar removed.

| Surface construct | HIR form |
|---|---|
| `for x in xs` over a list or range | Counting loop with direct indexing |
| `for x in it` over other iterables | Loop calling `next()` and matching on the option |
| f-string | String builder: append literal parts and `Display` calls |
| `expr?` | Match on the result; early return on `Err` or `None` |
| `with e as n: body` | Evaluate `e`, bind `n` to its `enter()`, run body, call `exit()` on every exit edge |
| `a if c else b` | Conditional expression |
| Method call | Direct call to a resolved function, or a virtual call with a slot index |
| Operators on user types | Calls to trait methods |
| Arithmetic on built-in numbers | Primitive operations; the `AddNumber` family is resolved away at compile time |
| Route annotation on a handler | A generated adapter function of type `http.Handler` that binds parameters from the request, calls the handler and encodes its result |
| `server.mount(value)` | One call-style route registration per annotated method of the value's type |
| Implicit coercions | Explicit `WrapSome`, `WrapOk`, `MakeTraitObject` nodes |
| Closure | Environment struct construction plus a lifted top-level function |
| Comprehension | Loop appending to a fresh list |
| Struct literal with defaults | All fields explicit |

Every HIR expression has a concrete or parametric type attached. After this phase no later phase needs to perform name lookup or inference.

### Checks beyond types

The type checker also enforces the rules that are not about type equality. Each is a local check with a dedicated diagnostic.

| Check | Rule | Where specified |
|---|---|---|
| Visibility | `pub` and `priv` | [01](01-language-design.md#visibility) |
| Frozen structs | No assignment to a field of a `@frozen` struct outside its construction | [01](01-language-design.md#structs-and-methods-m2) |
| Shareable at task boundaries | Arguments to `spawn`, channel element types, mutex contents, mounted values and route handlers | [04](04-runtime-and-memory.md#data-race-prevention) |
| Lock guard confinement | The name bound by `with lock as name` is not copied out of the block | [04](04-runtime-and-memory.md#data-race-prevention) |
| Route signatures | Every `{name}` in a path has a parameter; every parameter can be bound; at most one body parameter; the return type can be turned into a response | [05](05-standard-library.md#how-handler-parameters-are-bound) |

`Shareable` is a property computed per type and cached, like trait implementation lookups. Route checking and adapter generation run in the derivation pass, since an adapter is generated code just as a derived `Json` implementation is.

## Monomorphisation

A worklist starting from `main` and from every function whose address is taken by the runtime.

- For each reachable call to a generic function, substitute the type arguments into its HIR and add the instance to the worklist if it is new. The same applies to generic structs, enums and impls.
- Instances are keyed by (definition id, list of concrete type ids).
- Functions that are never reached are never compiled. This also serves as dead code elimination across the whole program.
- Vtables are generated for each (concrete type, trait) pair that appears in a `MakeTraitObject` node.

Symbol names follow a simple reversible scheme: module path, item name, and type arguments, for example `models.User.rename` and `core.first[int]`, escaped for the linker. A demangler is unnecessary because the names are readable as they are.

## LLVM code generation

### Type mapping

| Weby | LLVM |
|---|---|
| `int`, `float`, `bool` | `i64`, `double`, `i1` |
| Sized numerics | `i8` .. `i64`, `float` |
| `str`, `bytes` | `{ ptr, i64 }` |
| Struct value | `ptr` to a named struct type preceded by a one-word header |
| Tuple | Anonymous struct |
| Enum | `{ iN tag, [payload words] }`; pointer-like `Option` is a bare `ptr` |
| `list`, `dict`, `set` | `ptr` to runtime-defined layout |
| Trait object | `{ ptr data, ptr vtable }` |
| Function value | `{ ptr code, ptr env }` |
| No value | `void` |

### Functions and calls

- Each concrete function becomes one LLVM function. Internal functions use LLVM's `fastcc` and internal linkage so the optimiser is free to change their signatures; `main` and anything called from the runtime use the C calling convention.
- Small aggregates (strings, trait objects, function values, small enums) are passed in registers as separate scalars. Larger enums are passed by pointer to a caller-owned temporary.
- Closures and bound methods take the environment pointer as a hidden first argument. A plain function used as a value gets a tiny adapter that ignores it.
- Local variables are `alloca` slots in the entry block; LLVM's `mem2reg` promotes them to SSA registers. This avoids constructing phi nodes by hand.

### Allocation

Every heap allocation is a call to one runtime function with the size and a pointer to a constant type descriptor. Codegen picks the pointer-free variant for objects with no reference fields. Details in [04](04-runtime-and-memory.md#object-model).

### Safety checks

| Check | Emitted as | Removable |
|---|---|---|
| List and slice bounds | Compare and branch to a cold panic block | LLVM removes many in counted loops; `for x in xs` emits none |
| Integer overflow | Not checked. Arithmetic wraps in every build mode, as in Go, so plain LLVM `add`/`sub`/`mul` without the no-overflow flags | Nothing to remove |
| Division by zero | Compare and branch | Yes, when the divisor is a non-zero constant |
| `unwrap` on `None` or `Err` | Compare and branch | No |

Panic blocks call a `noreturn`, `cold` runtime function with a pointer to a static source location, which keeps the hot path compact.

### Control flow

- `if`, `while`, `for`: basic blocks and conditional branches.
- `and` / `or`: short-circuit branches.
- `match`: the decision tree from exhaustiveness checking becomes `switch` instructions on tags and comparisons on literals.
- `break` / `continue`: branches to the loop's exit or header block. `with` cleanup is duplicated on each exit edge in HIR, so codegen needs no special handling.

### Inline fast paths

Operations that are too hot to be runtime calls are emitted directly: list length and indexing, string length and equality prefix check, enum tag tests, `Option` checks, integer and float formatting buffers in f-strings. Growth, hashing and anything rare stays in the runtime.

### Optimisation and emission

- Debug builds: no optimisation passes beyond `mem2reg`, all checks on.
- Release builds: LLVM's standard `default<O2>` pipeline through the new pass manager, native CPU features optional behind a flag.
- Functions get accurate attributes where the compiler knows them (`nounwind` everywhere since there are no exceptions, `noalias` on fresh allocations, `readonly` on pure runtime helpers). These are cheap to add and help the optimiser.
- The module is verified with LLVM's verifier in debug builds of the compiler and in CI.
- Output: one object file through an LLVM `TargetMachine`, with position-independent code.

### Linking

The driver invokes the system C compiler as a linker driver (`cc`), passing the object file, `liblang_rt.a`, the GC library, and the system libraries the Rust runtime needs (threads, math, dynamic loading). The runtime archive is located relative to the `weby` executable. A fully static binary (`--static`, musl target) is a later option that suits container deployment.

### Debug information

Deferred to after the prototype, with one exception: panic messages carry file and line from M1, because they are static data and cost nothing. Full DWARF (so that `gdb`, `perf` and backtraces show Weby functions and lines) is the first post-prototype tooling task, since profiling needs it.

## Diagnostics

Good errors are part of the readability goal.

- Every diagnostic has a severity, a stable code (`E0123`), a primary span with a label, optional secondary spans, and optional help text.
- Messages state what was expected and what was found, in the language's own terms ("expected `User`, found `User?`; use `match` or `?` to handle `None`").
- Common Python habits get specific messages: `if items:` (truthiness), `class` (suggest `struct` and `impl`), `try` (suggest `Result` and `?`), `a < b < c`, `self.x` used without `self` parameter.
- Type hints get the same treatment: a parameter or field with no hint is an error that shows where to write it; `string`, `integer`, `boolean`, `List[...]`, `Dict[...]` and `Optional[...]` each produce an error naming the Weby spelling.
- Warnings: unused variable, unused import, unreachable code, naming convention.
- Diagnostics are golden-tested ([07](07-testing-and-performance.md)).

## Command line

| Command | Does | Milestone |
|---|---|---|
| `weby build file.wb` | Compile to an executable; `--release`, `-o` | M0 |
| `weby run file.wb [args]` | Build to a cache directory and execute | M0 |
| `weby check file.wb` | Phases 1–6 only; no LLVM | M1 |
| `weby build --emit=tokens\|ast\|hir\|llvm-ir\|obj` | Dump an intermediate form | M1, used heavily by tests |
| `weby test` | Run `@test` functions | post-prototype |
| `weby build --race` | Build with data-race detection instrumented | post-prototype |
| `weby fmt` | Formatter | post-prototype |

## Deferred compiler work

| Item | Trigger to start |
|---|---|
| Mid-level CFG IR | Escape analysis, precise GC, or yield-point insertion |
| Parallel codegen units, incremental builds | Clean build of a realistic service takes more than a few seconds |
| DWARF debug info | Immediately after the prototype |
| Language server | After the formatter; reuses `lang_syntax` and `lang_sema` |
| Cross compilation | When a second target is added |
