# Release polish plan: the language and the compiler

The plan for the last round of work before the final version is released, written for the agent that will implement it. The performance work is finished ([10](10-final-optimization-plan.md), D87 to D90). This round is about the language and the compiler: the gaps a Python programmer meets in the first hour, a crash that gives no message, and the tooling around them.

Planned on 2026-11-20 at commit `8b55b6a`; nothing here is implemented. Every gap below was found by compiling small programs against that commit; Appendix A lists them with the compiler's output.

## In short

- **Five milestones.** Each ends in a commit, a pushed branch (`main`), a decision record, and updated documents.
- **What was found.**
  1. **Syntax.** Common Python syntax is missing for no deep reason: one-line blocks (`if done: return`), raw strings (`r"\d+"`, needed by the `re` module), `del`, constant expressions (`const LIMIT = 4 * 1024`), `!r` in f-strings, `print(..., end="")`, and `match` as an expression.
  2. **Methods on built-in types.** `str`, `list` and `dict` have 13, 10 and 7 methods, and each one costs a checker case, a code generator case and a runtime function. A cheap way to write them in the language is missing.
  3. **Sets.** Every document says "not implemented yet; use a `dict[K, bool]`".
  4. **Generics and traits.**
     - Methods cannot have their own type parameters (D33).
     - A trait object cannot be matched against its concrete type, although [02](02-type-system-and-traits.md) planned it.
     - `xs = []` followed by `xs.append(1)` is an error.
  5. **Crashes and tooling.** A stack overflow ends the program with a bare segmentation fault and no message, on the main thread and in tasks. The language server has no completion. Some help messages are stale (one still promises `int.parse` "in milestone 3").
- **What is not here** (section 5 says why): generic traits, value types, `with` cleanup on panic, debug information for locals, a decision-tree `match`, a blocking pool, other platforms.

| Milestone | Theme | Main files | Size |
|---|---|---|---|
| [1](#milestone-1-syntax-gaps) | Syntax: one-line blocks, raw strings, `del`, constant expressions, f-string conversions, `print` keywords, `match` expressions, stale messages | `lang_syntax/src/{lexer,parser,ast}.rs`, `lang_sema/src/check/{stmt,expr,items,patterns}.rs`, `lang_cli/src/fmt.rs` | Medium |
| [2](#milestone-2-methods-of-the-built-in-types-written-in-the-language) | Methods of `str`, `list` and `dict` written in the prelude; `ord`, `chr`, `reversed`, `list(...)` | `lang_sema/src/check/builtins.rs`, `lang_sema/src/prelude.lang`, `lang_rt/src/strings.rs` | Medium |
| [3](#milestone-3-sets) | `set[T]`: literals, comprehensions, operators, methods, JSON; optionally `frozenset[T]` | `lang_sema/src/{types,check/*}.rs`, `lang_codegen/src/*`, `lang_rt/src/dict.rs` | Large |
| [4](#milestone-4-generics-and-traits) | Generic methods; type patterns on trait objects; empty collections typed by their first use | `lang_sema/src/check/{generics,items,patterns,stmt}.rs`, `lang_codegen/src/{traits,stmt}.rs` | Large |
| [5](#milestone-5-robustness-tooling-and-close-out) | Stack overflow message; optional loop preemption; completion in the language server; warnings; error reference; documents and release notes | `lang_rt/src/{fiber,lib}.rs`, `lang_codegen/src/stmt.rs`, `lang_cli/src/lsp.rs`, `docs/` | Medium |

Do them in this order. Milestone 3 uses the method mechanism from milestone 2 for most of the set methods. Milestone 4 is independent of 2 and 3 and can move earlier if it suits. Milestone 5's close-out comes last.

## 1. Goal and scope

**Goal:** the language behaves the way its documents and a Python programmer expect, in the places found by probing: no crash without a message, and no missing feature that its own documents call routine.

**In scope:** the lexer, parser, type checker, code generator, prelude and standard library sources, the formatter, the language server, the runtime where a feature needs it (sets, the stack overflow handler, preemption), the documents.

**Out of scope:** performance work, except that nothing may get slower (section 4); new standard library modules; changes to existing semantics. Every program that compiles today must compile and behave the same afterwards. The one exception is milestone 5's preemption, which changes scheduling but not results.

**Done means** for every item:
- parser tests where the syntax changed;
- at least one run test in `tests/run/`;
- at least one test in `tests/ui/` for each new error;
- [01](01-language-design.md), [02](02-type-system-and-traits.md) and [05](05-standard-library.md) updated in the same commit, since they are the language reference (roadmap working practices);
- [examples.md](examples.md) updated where it shows the old limitation. Its programs are compiled and run by `crates/lang_cli/tests/guide.rs`, so they must stay correct.

## 2. How the evidence was gathered

Programs in Appendix A were compiled with the release compiler at `8b55b6a` (`target/release/weby run`). Compile times are not a problem: checking any program in the repository takes 15 to 18 ms; building the todo service takes 0.18 s, or 0.29 s with `--release`. A hello-world executable is 5.2 MB, of which 3.8 MB is DWARF (`size -A`): 0.76 MB `.text`, 1.2 MB `.debug_loclists`, 1.1 MB `.debug_str`, 0.86 MB `.debug_info`. The diagnostics are already good in most places: typos get "did you mean", type mismatches point at the hint or parameter that set the expectation, and missing returns and unhashable keys are explained. The items below are the exceptions.

## 3. Conventions the new features must follow

These come from the existing documents. They settle naming and behaviour questions before they are asked.

- **Names.** Methods are `snake_case` (docs/01 line 20). Where Python runs two words together, the name gets an underscore, as `startswith` became `starts_with`. Where Python's name is one word or an established abbreviation, it stays (`strip`, `lstrip`, `find`). So `splitlines` becomes `split_lines`, `isdigit` becomes `is_digit`, `removeprefix` becomes `remove_prefix`. Every renamed method gets a "did you mean" hint from its Python name, as `startswith` has today (`builtins.rs:694`).
- **Missing things.**
  - A lookup that may fail returns an optional, as `dict.get` and `dict.pop` do.
  - An operation that Python makes raise on bad input panics, as `list.remove` of an absent value and `d[k]` of a missing key do. The message names the value: `panic: key 'q' is not in the dict`.
- **Integers wrap** (D16). Floats follow IEEE 754 (D23). Integer division by zero panics.
- **Printing** follows Python: `repr` inside containers, `str` at the top level.
- **Error codes.** New errors reuse an existing code when one fits (`codes.rs`). A new code goes at the end of its group and gets a line in the error reference (milestone 5).
- **Unsupported forms say so.** Something deliberately left out gets an `E0101`/`E0102` error that says what to write instead, as comparison chains do today.

## 4. Rules for the work

**Constraints** (as for the previous plans; they still hold):
- **No downloads** without the owner's approval. Cargo runs with `--offline`. Adding a crate to `Cargo.toml` needs approval even when the crate is already in the local registry cache.
- **The language's name and file extension** are never spelled in Rust source outside `crates/lang_names`, comments included. Use `CLI_NAME`, `SOURCE_EXTENSION` and `lang_names::env_var(...)`. Test programs, the prelude, standard library sources and documents may use them.
- **`tests/run/hello`** is an untracked file that is not part of the project. Do not commit it.
- **Commit and push after each milestone**, on `main`.

**Correctness gates, for every commit:**

```bash
cargo test --workspace --offline
```

```bash
sh tests/stress.sh 300
```

```bash
cargo clippy --workspace --all-targets --offline -- -D warnings
```

```bash
cargo fmt --check
```

`cargo test` includes these checks:
- every program in `tests/run/` in five modes (debug, release, and three collector stress modes);
- every `tests/ui/` diagnostic, byte for byte;
- the guide's programs;
- the formatter over every source file in the repository: it must keep each file's tokens and be idempotent.

New syntax must therefore be taught to the formatter (`crates/lang_cli/src/fmt.rs`, which scans text itself, D68) in the same commit.

**No slowdown.** Run the performance checks at the end of each milestone, on a quiet machine, never while building or testing:

```bash
python3 bench/run.py --check
```

```bash
python3 bench/http/counts.py --check
```

None of milestones 1 to 4 should move them, because they add features rather than change existing code paths. A failure means something changed by accident; find it rather than re-recording. Milestone 5's preemption is the one item expected to cost something, and it has its own gate.

**Decision records:** one per milestone in [08](08-decisions-and-risks.md), D91 to D95, in the style of D87 to D90. Each says what was done and what was decided (names, syntax, semantics), what was tried and dropped, and which earlier decision it amends: D31 (`match`), D33 (generic methods), D66 (stage 2 omissions), D68 (formatter), D70 (language server).

**Progress table:** add a row for this plan to the progress table in [06-roadmap.md](06-roadmap.md) at the end of each milestone.

## Milestone 1: Syntax gaps

Mostly parser and checker work. Each item is independent; do them in the order given, since the early ones are the most used.

### 1.1 One-line blocks

**Evidence:** `if True: print("x")` and `case Ok(v): print(v)` are errors ("expected a new line after `:`"). In a `match`, the failure also produces a spurious second error, "a `match` needs at least one `case`" (Appendix A, b5).

**Change:** after the `:` of `if`, `elif`, `else`, `while`, `for`, `case`, `with` and `def`, allow one *simple* statement on the same line instead of a newline and an indented block. The block consists of that one statement. The simple statements are:
- an expression;
- an assignment or augmented assignment;
- `return`, `pass`, `break`, `continue`;
- `assert`, and `del` from 1.3.

A compound statement after the colon stays an error, with help to put it on the next line. `;` to join statements is not added.

**Where:** `Parser::block_start` and `Parser::block` (`parser.rs:207`, `:925`). Suppress the "needs at least one `case`" error when a case failed to parse, which also helps other recovery paths.

**Formatter:** keep a one-line block on one line (D68 never moves code between lines). Normalise the spacing to one space after the colon.

**Tests:**
- parser tests for each statement kind;
- `tests/run/one_line_blocks.wb`;
- a `tests/ui/` case for a compound statement after a colon;
- a formatter case.

### 1.2 Raw strings and bytes literals

**Evidence:** `r"\d+"` is "raw strings are not supported yet", followed by "unknown escape sequence `\d`". The `re` module exists (D76), so every pattern needs doubled backslashes today.

**Change:**
- **Raw strings:** `r"..."`, `r'...'` and their triple-quoted forms, with Python's rules. A backslash is kept as is. A quote can be escaped but the backslash stays. A raw string cannot end in an odd number of backslashes.
- **Bytes literals (optional):** `b"..."` and `rb"..."` become `bytes` values, built through the prelude's `__bytes` (D57) from a string of the literal's bytes. Only ASCII characters and `\xNN` escapes are allowed, as in Python.
- **Still errors:** raw f-strings (`rf"..."`), with the existing message.

**Where:**
- the lexer, `lexer.rs:210` to `:230`;
- the formatter's scanner;
- the editor grammars in `editors/` (TextMate, Vim, tree-sitter, Zed), which should highlight the new prefixes.

**Tests:**
- lexer tests, including the edge cases;
- `tests/run/raw_strings.wb`, using `re` with a raw pattern;
- the existing `tests/ui/` case for unsupported prefixes, updated.

### 1.3 `del`

**Evidence:** `del xs[0]` is a parse error ("expected end of line").

**Change:** `del target` as a statement, desugared in the checker:
- `del xs[i]` becomes the list's pop-at with the result discarded (negative indices as in reads; panics when out of range);
- `del d[k]` becomes a removal that panics with the missing-key message when `k` is absent.

`del name`, `del obj.field` and `del xs[a:b]` are errors that say what to use instead. Make `del` a keyword (check that no test program or standard library file uses it as a name).

**Where:**
- `ast.rs`, `parser.rs`;
- `check/stmt.rs`;
- the runtime's dict removal entry point, for a variant that panics (or the code generator panics on an empty result).

**Tests:** `tests/run/del.wb`, and `tests/ui/del_errors.wb`.

### 1.4 Constant expressions

**Evidence:** `const LIMIT = 4 * 1024` is E0312, "computed constants are not supported yet".

**Change:** a constant's value may be an expression of:
- literals and other constants (in this module or imported);
- the unary and binary operators on `int`, `float`, `str` and `bool`;
- parentheses.

It is folded at check time to a literal with the runtime's exact semantics:
- integers wrap;
- `//` and `%` floor;
- `**` with a negative exponent is an error, as at run time;
- float printing is not involved.

Division by zero in a constant is a compile error. A cycle between constants is an error that shows the chain. Calls, collections and f-strings stay errors, with the message updated to say what is allowed.

**Where:** the constant collection in `check/items.rs`, where E0312 is reported, and a small folder over the AST. The HIR keeps getting a literal, so code generation does not change.

**Tests:** `tests/run/const_expressions.wb` (including wrap-around and cross-module constants), and `tests/ui/const_errors.wb`.

### 1.5 f-string conversions

**Evidence:** `f"{name!r}"` is "conversions such as `!r` in f-strings are not supported yet".

**Change:** `!r` formats with the representation used inside printed containers (`'x'` for a string, the same as `print([x])`). `!s` is the default and is accepted. `!a` stays an error. A conversion may be followed by a format spec (`{x!r:>10}`), as in Python.

**Where:** the f-string lexing and checking. The runtime already has the representation writer (`builder.rs`, `write_str_repr` and the container printing).

**Tests:** `tests/run/fstring_conversions.wb`.

### 1.6 `print` keywords

**Evidence:** `print(c, end="")` is "`print` takes no keyword arguments". There is no way to print without a newline.

**Change:** `sep: str` and `end: str` keywords, with Python's defaults. `file=` and `flush=` stay unsupported, with help pointing to `io` for standard error.

**Where:** `check/builtins.rs` and the `print` lowering in the code generator.

**Tests:** extend an existing print test, or add `tests/run/print_keywords.wb`.

### 1.7 `match` as an expression

**Evidence:** `y = match x:` is "expected an expression, found `match`". docs/01 line 263 defers it.

**Syntax:** allowed only as the whole right-hand side of an assignment (with or without a type hint) or of a `return`. Each case is one line, and its body is an expression:

```python
label = match status:
    case 200: "ok"
    case 404: "not found"
    case _: "other"
```

**Rules:**
- The cases must be exhaustive. A missing case is the existing non-exhaustive error, since the expression has no value to fall back on.
- The case types unify as the branches of `a if c else b` do (`IfElse` in the checker): an expected type from a hint or return type first, then the first case's type.
- Guards (`case x if x > 0: ...`) work as in statements.

**Lowering:** to the existing `hir::ExprKind::Block`: declare a temporary, compile a `match` statement whose cases assign it, and yield the temporary. Code generation then needs no change.

**Where:**
- `parser.rs`: the assignment and `return` statements look for `match` after `=` / `return`;
- `ast.rs`;
- `check/patterns.rs` (`match_stmt`);
- the formatter.

**Tests:**
- parser tests;
- `tests/run/match_expression.wb`, with enums, `Result`, literals and guards;
- `tests/ui/match_expression_errors.wb`: not exhaustive, mismatched case types, and `match` in other expression positions with help.

### 1.8 Stale and missing help

- `builtins.rs:190`: "parsing arrives as `int.parse(text)` with `Result` in milestone 3". It should say to use `int.parse(text)`, which returns a `Result`. Search every diagnostic string in `lang_syntax`, `lang_sema` and `lang_codegen` for "milestone", "yet" and "for now", and make each one true at the end of this plan.
- **Field access on an optional.** `u.name` where `u: User?` says only "`User?` has no field `name`". Add the help the method case already has (`builtins.rs:697`): check `if u is not None:` first, or use `.unwrap()`.
- **A misspelt keyword argument.** `User(nmae="a", age=1)` gives two errors: missing field `name`, and no field `nmae`. When exactly one field is missing and exactly one unknown name is close to it, report one error: "no field `nmae`; did you mean `name`?".
- **Built-in type names in "did you mean".** `list[Strr]` gets no suggestion. Add the built-in type names to the candidates of the unknown-type error.
- **Keep the planned lists current.** `PLANNED_FUNCTIONS` (`check/expr.rs:17`) and `PLANNED_TYPES` (`check/mod.rs:99`) name things that are "not available yet". Remove each one as it lands in this plan, and check the remaining messages point to what exists. For example, `input` should point to `io.read_line()` and `open` to `io.open(path)`, which both exist.

**Tests:** the affected `tests/ui/*.stderr` files are regenerated, and each changed text is reviewed by reading it.

### Done

Carried out on 2026-11-20 as D91 in [08](08-decisions-and-risks.md). All of 1.1 to 1.8 are done, with two differences: bytes literals (`b"..."`, optional in 1.2) are not implemented, since a `str` literal cannot hold invalid UTF-8, and the tree-sitter grammar was not regenerated, since that needs the `tree-sitter` command. `del` also became legal as a one-line block statement, and constants are checked on first use so that their order does not matter. Measured with `bench/run.py --check` and `bench/http/counts.py --check`: no regression and no count grew.

### Exit criteria

- Every program in Appendix A's milestone 1 rows compiles and prints what Python would print, except comparison chains, which stay an error by design.
- All gates pass.
- D91 is recorded.
- docs/01 has sections for one-line blocks, raw strings, `del`, constant expressions and `match` expressions. The sentence "Expression form is deferred" is gone.

## Milestone 2: Methods of the built-in types, written in the language

### 2.1 The mechanism

**The cost today:** each method of `str`, `list` and `dict` is:
- a case in the checker's signature table (`builtins.rs:524` onwards);
- a `hir::Builtin` operation;
- a case in `lang_codegen/src/builtins.rs`;
- usually a runtime function.

That is why there are so few. User code cannot add methods to built-in types (`items.rs:678`), and should not be able to.

**Change:** let the prelude define them as ordinary functions, found by name.
- **Lookup.** When `builtin_method` (`builtins.rs:502`) finds no compiler-defined method, it looks for a prelude function named `__<kind>_<method>`:
  - `__str_count`, `__list_count`, `__dict_get_or_insert`, later `__set_union`;
  - the kind prefixes are `str`, `list`, `dict`, `set`, `bytes`, `tuple` and `frozendict`.
- **The call.** The receiver becomes the first argument, and the call is checked like any other call. Generic prelude functions (`def __list_count[T: Eq](items: list[T], value: T) -> int`) get their type arguments from the existing inference. This is how `text.encode()` already reaches `__bytes` (`builtins.rs:515`), generalised.
- **Order.** Compiler-defined methods win, so the fast paths stay.
- **Names in diagnostics** must not show the prelude name. Arity errors, type mismatches and hover should say `list.count`, not `__list_count`. Look at how errors for calls name their callee, and pass the display name through.
- **"Did you mean"** in `unknown_builtin_method` (`builtins.rs:669`) should draw from both the compiler's table and the prelude's `__<kind>_` functions, instead of the hard-coded `STR_METHODS` and others.
- **Keyword and default arguments** work, since these are ordinary functions.
- **Visibility.** `__`-prefixed names must stay invisible to user code as plain names, as they are today. Check this with a `tests/ui/` case.

**Alternative, if the above proves awkward:** allow `impl str:`, `impl[T] list[T]:` and `impl[K, V] dict[K, V]:` in the prelude only, using the standard library flag (`check/mod.rs:188`). That is closer to how a user would read the prelude, but needs impl templates keyed on built-in type constructors in `check/generics.rs`, which today keys them on generic structs and enums. Choose after reading `generics.rs`, and record the choice in D92.

**Speed:** methods written in the language compile into the same LLVM module as the program (D12) and inline like user code. Methods that need byte-level work go through small runtime functions declared with `@extern` in the prelude, as `bytes` does (D57). Examples are character classes and substring counting with `memchr`/`memmem`.

### 2.2 The methods

Add these, each with a "did you mean" from the Python spelling where the name differs:

| Type | Methods | Notes |
|---|---|---|
| `str` | `count(sub)`, `rfind(sub)`, `index(sub) -> int?`, `split_lines()`, `partition(sep) -> tuple[str, str, str]`, `rpartition(sep)`, `remove_prefix(p)`, `remove_suffix(s)`, `is_digit()`, `is_alpha()`, `is_alnum()`, `is_space()`, `is_upper()`, `is_lower()`, `title()`, `capitalize()`, `zfill(width)`, `strip(chars)` and `lstrip`/`rstrip` with an optional argument, `split(sep, limit)` and `rsplit(sep, limit)` | `index` returns an optional rather than panicking, unlike Python, since `find` already gives -1 and an optional is the language's way. Decide the classes (`is_digit` and the others) for Unicode, as Python does, using Rust's `char` methods in the runtime; record it in D92 |
| `list` | `count(x)`, `xs * n` (and `n * xs`), `sort(key=..., reverse=...)` already exists, so check `reverse=` alone works too | `*` needs a checker case in the binary operator rules (today E0321). It can call a prelude `__list_repeat` |
| `dict` | `get_or_insert(key, default) -> V` (Python's `setdefault`), `update(other)`, `copy()`, `pop_or(key, default) -> V` | `setdefault` gets a "did you mean `get_or_insert`" hint |
| built-ins | `ord(c) -> int`, `chr(i) -> str`, `reversed(xs) -> list[T]`, `list(x) -> list[T]` for a list, `str` (its characters), a dict (its keys), `range(...)` and, after milestone 3, a set | `chr` panics outside the Unicode scalar range. `range(...)` outside a `for` loop stays an error, with help that now suggests `list(range(...))`. `for x in reversed(xs)` may iterate backwards without building a list, as a later refinement only if it is a small change |

**Tests:**
- `tests/run/str_methods.wb`, `tests/run/list_methods.wb`, `tests/run/dict_methods.wb`, each comparing with Python's results for the same inputs (write the expected lines from running the Python equivalent by hand);
- `tests/ui/builtin_method_hints.wb` for the Python spellings.

**Documents:**
- the `str`, `list` and `dict` rows of the table in docs/05;
- docs/01 line 116;
- the method lists in examples.md.

### Done

Carried out on 2026-11-20 as D92 in [08](08-decisions-and-risks.md), by the first mechanism in 2.1: prelude functions found by name, not `impl` blocks. Differences from the plan: `str.index` panics rather than returning an optional (the list's `index` panics, and D92 gives the reasons); the hidden `__at` parameter gives prelude panics the caller's location, which the plan did not foresee; the `__` names are not hidden from user code, since generated code uses them; `zfill`, `partition` and `rpartition` follow Python exactly, and `split_lines` handles only `\n`, `\r\n` and `\r`.

### Exit criteria

- The methods above exist, written in the prelude except where a runtime function is needed for speed or Unicode.
- No compiler-defined method was removed or slowed (`bench/run.py --check`).
- D92 is recorded, with the mechanism chosen and the naming decisions.

## Milestone 3: Sets

**Evidence:** `{1, 2, 3}` is "set literals are not supported yet". docs/01, docs/05 and examples.md all tell users to use `dict[K, bool]`.

### 3.1 The language

- **Type.** `set[T]` where `T: Hash` (the same rule as dict keys, E0313).
- **Making sets.**
  - Literals: `{1, 2}`. `{}` stays an empty dict, as in Python. An empty set is `set()` with an expected type, or `x: set[int] = set()`.
  - `set(xs)` from a list, `str`, dict (its keys) or set.
  - Comprehensions: `{x for x in xs if ...}`.
- **Operations.**
  - `len`, `in`, `not in`, iteration (insertion order, since the dict is ordered).
  - `add(x)`; `remove(x)`, which panics when absent, like `list.remove`; `discard(x) -> bool`; `clear()`; `copy()`; `pop() -> T?`.
  - `|`, `&`, `-`, `^`, and the methods `union`, `intersection`, `difference`, `symmetric_difference`, `is_subset`, `is_superset`, `is_disjoint`.
  - `==` and `!=`. `<=` and `<` as subset tests are optional; Python has them, but they make `Ord`-like sorting of sets meaningless, so leaving them out is defensible. Decide and record it.
- **Printing** as Python does: `{1, 2}`, and `set()` when empty.
- **JSON.** A set encodes as an array. A `set[T]` field decodes from an array, and duplicates are allowed, as Python's `set(json.loads(...))` would treat them.
- **Sharing.**
  - A `set` is mutable and not `Shareable` (D44), like `list` and `dict`.
  - Optionally, `frozenset[T]` is the immutable, shareable variant, following `frozendict` (`TyKind::FrozenDict`). It is in `PLANNED_TYPES` and in docs/01's table. Include it if it is mostly a copy of the `frozendict` cases; otherwise leave it with its "not supported yet" error and say so in D93.
- **`Hash` for sets:** not provided, as in Python. A set cannot be a dict key or a set element.

### 3.2 The implementation

**Type and representation:** a new `TyKind::Set(Ty)` in `lang_sema/src/types.rs`. Represent a set as a dict whose values are never read, so that the dict runtime, its typed `str` and `int` entry points (D90) and its iteration are reused as they are. Check whether `lang_rt/src/dict.rs` can store a zero-sized value. If it cannot, use the smallest value it supports and note the memory cost in D93.

**Sites to update:** every place that matches `TyKind::Dict` must consider `Set`. There are 47 in `lang_sema` and `lang_codegen` today, and 27 for `FrozenDict`. Go through them one by one:
- type display and parsing of `set[...]`;
- equality, printing, iteration, `in`;
- `Shareable`;
- JSON encode and decode;
- the language server's hover;
- comprehensions;
- the collector descriptors, since the elements must be scanned.

**Methods:** written as prelude `__set_*` functions (milestone 2) wherever they can be built on `in`, `add` and iteration. The operators `|`, `&`, `-` and `^` lower to those functions.

**Planned lists:** remove `set` and `frozenset` (if done) from `PLANNED_TYPES` and `PLANNED_FUNCTIONS`.

### 3.3 Tests

- **Run test.** `tests/run/sets.wb` covers every operation above, sets of `str`, `int`, tuples and a `@derive(Hash, Eq)` struct, a large set that grows through several resizes, and removal during a loop over a copy. It runs in all five modes, including collector stress.
- **Errors.** `tests/ui/set_errors.wb` covers:
  - a set of lists (unhashable);
  - `{}` assigned to a `set` hint, with help to write `set()`;
  - a set sent to a task (not shareable);
  - a set used as a dict key.
- **JSON.** A round trip in `tests/run/json_roundtrip.wb`, or a new file.

### Exit criteria

- All of the above works.
- examples.md's "Coming from Python" row for `set` describes sets.
- docs/01's types table and docs/05 have `set[T]`.
- `bench/run.py --check` passes.
- D93 is recorded.

## Milestone 4: Generics and traits

### 4.1 Methods with their own type parameters

**Evidence:** `def map[U](self, f: def(T) -> U) -> Box[U]` in `impl[T] Box[T]` is "methods cannot have their own type parameters yet" (D33). The error then cascades into "`Box[int]` has no method `map`" and "cannot infer the types of this lambda's parameters" (Appendix A, a5).

**Change:** allow type parameters on methods in inherent `impl` blocks, generic or not. They are checked once against their bounds and instantiated per use, exactly as top-level generic functions are (D32). An instance is keyed by the impl's type arguments and the method's own.
- **Inference:** from the arguments and the expected type, with lambdas last, as for functions.
- **Explicit type arguments:** `box.map[str](f)`, if the parser can take it without ambiguity against indexing. If it cannot, leave explicit arguments out and say so.
- **Trait methods** may not have type parameters; this keeps trait objects possible (docs/02, object safety). That gets its own error.

**Cascade:** until a method is accepted, a method declaration that fails for this reason should still be declared (with its parameters as error types), so that calls to it do not produce the two follow-on errors. This matters for any future declaration error too.

**Where:** `check/generics.rs` and `check/items.rs`. Bound methods used as values (D36) must work for instantiated generic methods, or give a clear error.

**Tests:**
- `tests/run/generic_methods.wb`: `map`, `fold` and a method using both the impl's and its own parameters;
- `tests/ui/generic_method_errors.wb`: unsatisfied bound, a trait method with type parameters, and a type argument that cannot be inferred.

### 4.2 Type patterns on trait objects

**Evidence:** `match s:` where `s: Shape` is "cannot `match` on a `Shape` trait object yet". docs/02 (Dynamic dispatch) planned `case Circle(c):` "for after M4".

**Change:** when the subject of a `match` is a trait object, a case may be a type pattern:
- `case Circle(c):` binds `c: Circle` to the concrete value;
- `case Circle():` tests without binding;
- `case Circle(c) | Square(c)` is not allowed, since `c` would have two types.

The type must be a struct or enum that implements the trait (or a trait that requires it); otherwise the pattern is an error that can never match. Trait objects are open, so the cases are never exhaustive without `case _:`. A missing `_` is the existing non-exhaustive error, with help to add `case _:`.

**Lowering:** slot 0 of every vtable is the concrete type's descriptor (`traits.rs:219`, `entries = vec![self.desc(ty)]`), and that holds for supertrait vtables too. The test is therefore one load from the vtable and one pointer comparison with `self.desc(Circle)`. The bound value is the trait object's data pointer, retyped. Make sure `desc(ty)` is unique per type across the module, as it is used for this identity. Generic struct instances each have their own descriptor; `case Box(b)` on a generic `Box` needs the type arguments written out (`case Box[int](b)`) or is an error. Decide which and record it.

**Tests:**
- `tests/run/trait_type_patterns.wb`: several types, a supertrait object, and a guard;
- `tests/ui/trait_type_pattern_errors.wb`: a type that does not implement the trait, a missing `case _`, and an or-pattern.

### 4.3 Empty collections typed by their first use (optional)

**Evidence:** `xs = []` followed by `xs.append(1)` is "cannot infer the type of an empty list". This is the most common Python idiom the language rejects; the help (`items: list[int] = []`) is good but the error is frequent.

**Change:** when a local is assigned an empty list, dict or (after milestone 3) `set()` with no expected type, give the local a pending type instead of reporting E0315 at once. Pending types are a new `TyKind` that only such locals ever carry. The first use that fixes the element types resolves it:
- `x.append(e)`, `x.insert(i, e)`, `x.extend(e)`;
- `x[k] = v`;
- `x.add(e)`;
- `return x` in a function with a declared return type;
- passing `x` as an argument whose parameter type is known;
- assigning `x` to a variable or field with a declared type.

Once resolved, the local has its real type for the rest of the function, and the HIR nodes created before resolution (the literal, and the receiver of the resolving call) are patched. Any other use before resolution is E0315 with the existing help. So is reaching the end of the function unresolved, or a branch where two different first uses would resolve it differently.

**Abort criterion:** this touches the type checker's core. If it needs changes beyond:
- the local table;
- the assignment checker;
- the method-call checker for the resolving forms;
- argument checking;
- one HIR patching walk,

stop. Keep E0315 as it is, and record in D94 why it was left. Do not let a half-working version through: a test program must show that every resolving form and every error case behaves as described.

**Tests:** `tests/run/empty_collection_inference.wb`, and `tests/ui/empty_collection_errors.wb` (used before resolution, never resolved, conflicting resolutions).

### Exit criteria

- 4.1 and 4.2 work. 4.3 works or is recorded as left out.
- D33's "Methods do not have their own type parameters yet" is amended by D94.
- docs/02 describes generic methods and type patterns, and no longer lists them as planned.
- `bench/run.py --check` passes.

## Milestone 5: Robustness, tooling and close-out

### 5.1 A message for stack overflow

**Evidence:** unbounded recursion ends the program with "Segmentation fault" and status 139, both on the main thread and in a task (Appendix A, d1 and d2). D66 lists this as a known omission.

**Change:** a `SIGSEGV`/`SIGBUS` handler on an alternate signal stack (`sigaltstack`, set up for the main thread at startup and for each worker thread when it starts). It checks whether the faulting address (`si_addr`) is in:
- the running fiber's guard page (the lowest page of its mapping, `fiber.rs:319` and `:573`);
- or just below the main thread's or a worker thread's stack (from `pthread_getattr_np`).

**If it is,** the handler writes a message with `write(2)`, which is async-signal-safe. The message is `panic: stack overflow`, followed by `in a task` when on a fiber, and the advice to look for unbounded recursion or to set the fiber stack size (`WEBY_TASK_STACK`, through `lang_names::env_var`). It then exits with status 101, the panic status (D24). Unwinding from a signal handler is not safe, so this ends the process even in a task. Say so in D95; turning it into a task-scoped panic is out of scope.

**If it is not,** the handler restores the previous handler and returns, so the fault happens again and behaves as it does today (a core dump for a real bug).

**Install** the handler after the collector is initialised. Check that Boehm does not rely on its own `SIGSEGV` handler in the configuration used: it uses signals to stop threads, but not `SIGSEGV`, unless incremental mode is on, which it is not.

**Tests:** two `tests/run/` programs with `# expect-exit: 101` and `# expect-stderr:` lines (the harness supports both, `programs.rs:87`): deep recursion on the main thread, and in a spawned task. Make sure they run quickly in debug mode and under collector stress. Use `# env: WEBY_TASK_STACK=...` for a small fiber stack if needed.

### 5.2 Preemption at loop back-edges (optional, measurement-gated)

**Evidence:** D66: a fiber runs until it waits. A handler stuck in a long computation holds its worker, and as many such handlers as cores stop the server. This is the one robustness gap a production web service would hit.

**Change:**
- **The flag.** Each worker has a "should yield" flag. The timer thread already wakes regularly to sweep deadlines (D65). It sets the flag for a worker whose running fiber has not changed since its previous look, using a per-worker switch counter it compares.
- **Generated code.** At every loop back-edge (`while`, `for`, comprehensions), load the current worker's flag and branch, marked unlikely, to a cold call to a new runtime function. That function clears the flag, puts the fiber at the back of the run queue and switches.
- **The main thread** (not a fiber) has a flag that is never set.
- **The tick.** The sweep's period (250 ms) bounds the delay. Check whether a shorter tick, only while some worker is busy, costs anything measurable. Go uses about 10 ms.

**Gate:**
- `bench/run.py --check` must pass with this on.
- An A/B run (`bench/ab.py`) of `sieve`, `word_count`, `sort` and `json_orders` must show no more than 2% loss.

If it costs more, try reading the flag through a register-pinned or global address rather than thread-local storage. If it still costs more, leave it out and record the measurements in D95.

**Tests:** a `tests/run/` program with `# env: WEBY_WORKERS=1` (the harness supports `# env:` lines, `programs.rs:5`). A spawned task spins in a loop for about a second while another task ticks with `time.sleep`. The ticks must interleave with the spin.

### 5.3 Completion in the language server

**Evidence:** `weby lsp` offers diagnostics, hover, go to definition and formatting; completion is missing (D70).

**Change:** `completionProvider` with `.` as a trigger character.
- **After `.`:** the fields and methods of the receiver's type: struct fields, inherent and trait methods, the compiler-defined built-in methods, and milestone 2's prelude methods under their user-facing names. Module members after a module name.
- **Elsewhere:** the locals, parameters, functions, types and modules in scope, and the keywords.

While typing, the text usually does not parse (`user.` is incomplete). Keep the last analysis that succeeded, and find the receiver by the position of the expression that ends just before the dot in that analysis. If the line was edited since, fall back to the names in scope. Each item has a kind (field, method, function, variable, module, keyword) and its type as `detail`.

**Where:** `crates/lang_cli/src/lsp.rs`, and the index built by the checker (D70).

**Tests:** extend the scripted session in `lsp.rs` with:
- a completion after `.` on a struct, on a `str`, and on a module;
- a completion of a local name;
- a completion in a document that does not parse.

### 5.4 Warnings

- **Unused import (new warning W0004).** `import json` or `from x import y` with no use in the file. The help shows the line to delete. The standard library's own files must not trigger it; check them.
- **Unused private function (optional).** A module-level `def` that is neither `pub`, nor called, nor `main`, nor `@test`, nor used as a value. Only if it can be computed reliably across the generated route adapters (D52), which call user functions from generated code.

**Tests:** a `tests/ui/` case for each.

### 5.5 Error reference

Add `docs/errors.md`. It has one section per code in `crates/lang_diag/src/codes.rs`, each with what it means, a short example taken from `tests/ui/`, and how to fix it. Add a test in `lang_diag` or `lang_cli` that fails when a code has no section, so the reference cannot fall behind.

A `weby explain E0315` command that prints the section is optional. If it is added, embed the file with `include_str!`.

### 5.6 Executable size (optional)

**Evidence:** a hello-world executable is 5.2 MB, of which 3.8 MB is debug sections (section 2). The program's own line tables are needed for panic backtraces and `perf` (D67).

**Investigate** which object files the DWARF comes from: `readelf --debug-dump=info` and the `DW_AT_producer` and `DW_AT_name` entries. If most of it is the runtime's and the Rust standard library's location lists and strings, try these, in order:
1. strip the runtime archive's debug information except line tables, at build time;
2. a `--strip` build option that removes everything but what backtraces need.

Panic backtraces must still name the program's functions and lines (there is a test for it). Record the sizes before and after in D95. If neither saves at least 1 MB, leave it.

### 5.7 Close-out

1. **Re-run the probes.** Re-run every program in Appendix A, and put the new outputs in D95: what now works, and what is still refused by design.
2. **Reference documents.** Go through docs/01, docs/02, docs/05 and examples.md once more as a reader. Every "not yet", "deferred" and "planned" must still be true. Where a feature landed, the text describes it. Where it did not, the text says so plainly.
3. **Roadmap and README.** Add the progress row in docs/06. Update the README's feature list.
4. **Release notes.** Write `docs/release-notes.md`: what the language has, in a short list per area. Cover the language, the standard library, concurrency, the web stack, tooling and performance (link [web-performance.md](web-performance.md)). Then the known limitations, taken from docs/08's decisions that are still open (D45, D64, D66, D67, D68, D71) and from section 5 below. Then how to install it (docs/building.md).
5. **Two decisions for the owner**, not to be taken without asking:
   - **Version.** Whether to change the version in `Cargo.toml` (now `0.1.0`) for the release.
   - **Continuous integration.** Whether to add a GitHub Actions workflow now that the repository has a remote. The roadmap's M0 asked for it, and docs/06 records it as not set up because there was no remote. A workflow runs on GitHub and installs LLVM 19 and Boehm there, so it is outward-facing. Write the file only if the owner agrees.
6. **Final checks.** Run the performance checks, then commit and push.

### Exit criteria

- Stack overflow gives a message and status 101.
- Completion works in the scripted session.
- Every code has a section in docs/errors.md.
- The release notes exist.
- D95 is recorded with the optional items' outcomes.
- All gates and both performance checks pass.

## 5. Not in this pass

Each of these is real work that a later version could take on. None is a polish item.

- **Generic traits** (`Iterator[T]`, `Iterable[T]`, `Scoped[T]`; D34 and D38). The method-name protocols give users the same syntax and static dispatch today. Generic traits need associated-type-like inference through trait objects and bounds, which is the largest remaining type system project.
- **`with` cleanup on panic** (D64). It needs landing pads in generated code. The runtime already releases mutexes held by a panicking task, which is the cleanup other tasks depend on.
- **Debug information for locals** (D67). Describing strings, optionals, results and closures to a debugger is its own project.
- **A decision-tree `match`** (D31). No profile has shown repeated tag tests.
- **A blocking pool for file I/O** (D66). Web services do little file I/O on request paths.
- **Value types and a precise collector.** See the optimisation handoff, section 11.
- **Comparison chains, `*args` and `**kwargs`, `global` and `nonlocal`, assigning to captured variables** (D35). These are deliberate omissions with good errors, not gaps.
- **Formatter line fitting** (D68), **incremental checking** in the language server (D70), **version solving** for dependencies (D71).
- **ThreadSanitizer** (D45), **Autobahn and JSONTestSuite downloads** (D59, D47), **other platforms** (D13). All need downloads or machines not available.

## 6. Risks

| Risk | Mitigation |
|---|---|
| One-line blocks or `match` expressions make the layout rules ambiguous | Restrict both to the forms in 1.1 and 1.7, and test the formatter's idempotence over every file, which `cargo test` already does |
| `__<kind>_<method>` names leak into diagnostics or hover | Milestone 2 requires display names. Add `tests/ui/` cases that call prelude methods wrongly and check the text |
| Sets touch about 75 type-kind match sites; one missed site panics in the code generator | Go through the sites one by one, and make the run test cover every operation in all five modes, including collector stress |
| Pending types for empty collections leak into code generation | 4.3 is optional and has an abort criterion. The HIR patching walk must assert that no pending type remains |
| The stack overflow handler misreads a real bug as an overflow, or hides one | It acts only on addresses in a guard page or just below a thread's stack, and otherwise re-raises with the previous handler |
| Preemption slows tight loops | It is optional and measured; a 2% bound, and it is dropped otherwise |
| Compiler-defined methods and prelude methods disagree on a name | Compiler-defined methods win, and a test lists every built-in type's methods through completion (5.3), which shows duplicates |

## Appendix A: The probes

Compiled with `target/release/weby` at `8b55b6a`. "Today" is the compiler's first error, shortened.

| Program | Today | Milestone |
|---|---|---|
| `if True: print("x")` | E0100 expected a new line after `:` | 1.1 |
| `case Ok(v): print(v)` | E0100 as above, plus a spurious "a `match` needs at least one `case`" | 1.1 |
| `p = r"\d+"` | E0101 raw strings are not supported yet, plus E0004 unknown escape `\d` | 1.2 |
| `del xs[0]` | E0100 expected end of line | 1.3 |
| `const LIMIT = 4 * 1024` | E0312 computed constants are not supported yet | 1.4 |
| `f"{'x'!r}"` | E0101 conversions such as `!r` are not supported yet | 1.5 |
| `print(c, end="")` | E0102 `print` takes no keyword arguments | 1.6 |
| `y = match x:` ... | E0100 expected an expression, found `match` | 1.7 |
| `int("42")` | E0102, help: "parsing arrives as `int.parse(text)` with `Result` in milestone 3" (stale) | 1.8 |
| `u.name` with `u: User?` | E0316 `User?` has no field `name`, no help | 1.8 |
| `User(nmae="a", age=1)` | Two errors: missing `name`, no field `nmae` | 1.8 |
| `xs: list[Strr] = []` | E0304 unknown type, no suggestion | 1.8 |
| `xs.count(1)`, `d.setdefault("k", 3)` | E0316 no method | 2 |
| `[0] * 3` | E0321 cannot use `*` on `list[int]` and `int` | 2 |
| `ord("a")`, `chr(98)`, `reversed(xs)`, `list(d.keys())` | E0200 unknown name | 2 |
| `s = {1, 2, 3}` | E0101 set literals are not supported yet | 3 |
| `def map[U](self, ...)` in an `impl` | E0102 methods cannot have their own type parameters yet, plus two follow-on errors | 4.1 |
| `match s:` with `s: Shape`, `case Circle(c):` | E0329 cannot `match` on a `Shape` trait object yet | 4.2 |
| `ys = []` then `ys.append("a")` | E0315 cannot infer the type of an empty list | 4.3 |
| `def deep(n): return deep(n + 1) + 1`, on the main thread or in a task | Segmentation fault, status 139, no message | 5.1 |
| `trait Container[T]:` | E0101 generic traits are not supported yet | Not in this pass |
| `1 < x < 3` | E0103, with help to write `and` | By design |
| `def f(*args: int)` | E0100 expected a parameter name | By design |
| Assigning a captured variable in a nested function | E0332, with help | By design (D35) |

Things that were checked and work: slicing and negative indices, `enumerate`, `zip`, `sorted(key=..., reverse=...)`, `list.sort(key=...)`, dict and list comprehensions with several `for` clauses, tuple unpacking, `?` on `Result` and on optionals, `x if c else y`, closures (late-binding loop variables, as in Python), Unicode iteration over `str`, byte-offset slicing with a clear panic inside a character, integer wrap-around, division by zero, the missing-key and out-of-range panics, and the typo suggestions for names, fields, types and Python's `startswith`.
