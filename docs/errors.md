# Error and warning reference

Every diagnostic the compiler gives has a code: `E` for an error, which stops the program from compiling, and `W` for a warning, which does not. The code is in the first line of the message, for example `error[E0300]: expected `int`, found `str``. This page says what each one means, shows a real message from the compiler's tests (the files named under each are in `tests/ui/`), and says what to do.

The message itself says more than the code can: it points at the place in the source, often at the declaration that caused the error, and its `help:` line says what to write. Read it first; come here for the background.

A test (`every_code_has_a_section` in `crates/lang_cli/tests/errors.rs`) fails when a code in `crates/lang_diag/src/codes.rs` has no section here, so this page cannot fall behind.

| Group | Codes | What they are |
|---|---|---|
| [Lexical](#lexical-errors) | E0001 to E0007 | The text is not valid tokens: tabs, strings, characters, numbers |
| [Syntax](#syntax-errors) | E0100 to E0104 | The tokens do not form a program |
| [Names](#name-errors) | E0200 to E0204 | A name that is missing, repeated or used too early |
| [Types and checking](#type-and-checking-errors) | E0300 to E0341 | Everything the type checker finds |
| [Warnings](#warnings) | W0001 to W0004 | Code that compiles but is probably a mistake |


## Lexical errors

### E0001: tab in indent

A tab character in the indentation at the start of a line.

From `tests/ui/tab_indentation.vi`:

```text
error[E0001]: tab character in indentation
 --> tests/ui/tab_indentation.vi:2:1
  |
2 |  print("tab")
  | ^ tab here
  |
  = help: indent with spaces only
```

**What to do.** Indent with spaces, four per level. A tab in the indentation is an error because its width differs between editors; most editors can be set to insert spaces for the Tab key.

### E0002: bad dedent

A line is less indented than its block but matches no enclosing block.

From `tests/ui/lexical_errors.vi`:

```text
error[E0002]: unindent does not match any outer indentation level
 --> tests/ui/lexical_errors.vi:8:1
  |
8 |   w = 4
  | ^^ this indentation
  |
  = help: indent this line by the same number of spaces as an enclosing block
```

**What to do.** Dedent to a column that an enclosing block uses. After a block of lines indented four spaces, the next line must be at the same four spaces or at an earlier level's column, not between.

### E0003: unterminated string

A string literal without its closing quote.

From `tests/ui/unterminated_string.vi`:

```text
error[E0003]: unterminated string literal
 --> tests/ui/unterminated_string.vi:2:11
  |
2 |     print("no closing quote)
  |           ^ string starts here
  |
  = help: close the string with a matching quote on the same line, or use `"""` for text spanning lines
```

**What to do.** Close the string with the quote that opened it, on the same line. Use triple quotes (`"""`) for text that spans lines.

### E0004: bad escape

An unknown backslash escape in a string literal.

From `tests/ui/lexical_errors.vi`:

```text
error[E0004]: unknown escape sequence `\q`
 --> tests/ui/lexical_errors.vi:4:15
  |
4 |     print("tab\q")
  |               ^^ unknown escape
  |
  = help: write `\\` for a literal backslash
```

**What to do.** The escapes are `\n`, `\t`, `\r`, `\0`, `\\`, `\'`, `\"` and `\u{...}`. To write a backslash itself, double it, or use a raw string, `r"\d+"`, in which nothing is an escape.

### E0005: unexpected char

A character that cannot start any token.

From `tests/ui/lexical_errors.vi`:

```text
error[E0005]: unexpected character `$`
 --> tests/ui/lexical_errors.vi:6:11
  |
6 |     y = 1 $ 2
  |           ^ not valid here
```

**What to do.** The character is not part of the language outside a string or a comment. Remove it, or quote it in a string. A backslash to continue a line is not supported: put the expression in parentheses.

### E0006: int too large

An integer literal that does not fit in 64 bits.

From `tests/ui/extern_and_with.vi`:

```text
error[E0006]: integer literal is too large for `u8`
  --> tests/ui/extern_and_with.vi:19:16
   |
19 |     tiny: u8 = 300
   |                ^^^ the largest `u8` is 255
```

**What to do.** Integers are 64 bits. For a bigger number use a `float`, or keep the digits in a `str`.

### E0007: bad number

A malformed number literal.

From `tests/ui/lexical_errors.vi`:

```text
error[E0007]: invalid number literal `12abc`
 --> tests/ui/lexical_errors.vi:5:9
  |
5 |     x = 12abc
  |         ^^^^^ not a valid number
```

**What to do.** A number is digits, optionally with `_` between them, a `.` and an exponent (`1_000`, `2.5`, `1e9`), or starts with `0x`, `0o` or `0b`. Letters stuck to the digits make it malformed; separate the name from the number.


## Syntax errors

### E0100: unexpected token

The parser found a token it did not expect.

From `tests/ui/lexical_errors.vi`:

```text
error[E0100]: expected a declaration, found name `w`
 --> tests/ui/lexical_errors.vi:8:3
  |
8 |   w = 4
  |   ^ statements are not allowed here
  |
  = help: statements must be inside a function; the program starts in `def main():`
```

**What to do.** The message says what was expected at that point. Often a `:` or a line break is missing, a bracket is not closed, or a block is not indented under its header.

### E0101: not supported syntax

Valid syntax for a feature this compiler does not implement yet.

From `tests/ui/fstring_conversion_errors.vi`:

```text
error[E0101]: the `!a` conversion is not supported
 --> tests/ui/fstring_conversion_errors.vi:4:15
  |
4 |     print(f"{x!a}")
  |               ^^ not supported
  |
  = help: use `!r`, which shows a string with quotes, as `repr` does
```

**What to do.** The syntax is valid Python but not in this language. The message says what to write instead, for example `a < b and b < c` for a comparison chain.

### E0102: not supported

A recognised construct that the type checker does not implement yet.

From `tests/ui/del_errors.vi`:

```text
error[E0102]: `del n` is not supported
 --> tests/ui/del_errors.vi:5:9
  |
5 |     del n
  |         ^ not supported
  |
  = help: a variable cannot be removed; leave it alone, or reassign it
```

**What to do.** A feature this compiler does not have. The help says what to use instead.

### E0103: comparison chain

`a < b < c`.

From `tests/ui/comparison_chain.vi`:

```text
error[E0103]: comparison chains are not supported
 --> tests/ui/comparison_chain.vi:2:11
  |
2 |     print(1 < 2 < 3)
  |           ^^^^^^^ second comparison
  |
  = help: write `a < b and b < c` instead of `a < b < c`
```

**What to do.** Write `a < b and b < c`; chained comparisons are not supported.

### E0104: outside loop

`break` or `continue` outside a loop.

From `tests/ui/break_outside_loop.vi`:

```text
error[E0104]: `break` outside a loop
 --> tests/ui/break_outside_loop.vi:3:5
  |
3 |     break
  |     ^^^^^ not inside `for` or `while`
```

**What to do.** `break` and `continue` only make sense inside a `for` or `while` loop. Move the statement into the loop, or use `return`.


## Name errors

### E0200: unknown name

A name that refers to nothing.

From `tests/ui/module_privacy.vi`:

```text
error[E0200]: module `lib.accounts` has no `open_acount`
 --> tests/ui/module_privacy.vi:2:52
  |
2 | from lib.accounts import Account, internal_helper, open_acount
  |                                                    ^^^^^^^^^^^ not found
  |
  = help: did you mean `open_account`?
```

**What to do.** Check the spelling (the help suggests the nearest name), and that the name is defined before use in that scope or imported. Python's names for functions that the language spells differently (`true` for `True`) get a hint.

### E0201: duplicate definition

Two definitions of the same name.

From `tests/ui/definition_errors.vi`:

```text
error[E0201]: `helper` is defined more than once
 --> tests/ui/definition_errors.vi:6:5
  |
3 | def helper() -> int:
  |     ------ first defined here
  ...
6 | def helper() -> int:
  |     ^^^^^^ defined again here
```

**What to do.** Give one of the two a different name, or remove the duplicate. A name taken by the language (`print`, `list`) cannot be defined.

### E0202: no main

The program has no `main` function.

From `tests/ui/no_main.vi`:

```text
error[E0202]: the program has no `main` function
 --> tests/ui/no_main.vi:1:1
  |
1 | def helper():
  | ^
  |
  = help: add `def main():`; the program starts running there
```

**What to do.** A program starts in `def main():`. Add one. A file meant as a module for other files does not need `main` and is checked without it by the language server.

### E0203: not a function

Calling something that is not a function, or assigning to a function.

From `tests/ui/generic_method_errors.vi`:

```text
error[E0203]: `Holder.smaller` needs a `Holder` value
  --> tests/ui/generic_method_errors.vi:30:18
   |
30 |     print(Holder.smaller(1, 2), holder.make(3))
   |                  ^^^^^^^ takes `self`
   |
   = help: call it on a value: `value.smaller(...)`
```

**What to do.** Only functions, methods, constructors and function values can be called; the message says what the name is. A struct is built with `Name(field=...)`, and a variant with `Enum.Variant(...)`.

### E0204: used before assigned

A variable read before its first assignment.

From `tests/ui/maybe_unassigned.vi`:

```text
error[E0204]: `result` might not be assigned here
 --> tests/ui/maybe_unassigned.vi:5:11
  |
4 |             result = "done"
  |             ------ assigned here, but not on every path to the use
5 |     print(result)
  |           ^^^^^^ may be unassigned here
  |
  = help: make sure every path assigns it first, for example by giving it a value before the `if` or loop
```

**What to do.** Assign the variable on every path before reading it, including both sides of an `if`. Give it a starting value before the `if` or loop.


## Type and checking errors

### E0300: type mismatch

A value of one type where another was expected.

From `tests/ui/builtin_method_hints.vi`:

```text
error[E0300]: expected `str`, found `int`
  --> tests/ui/builtin_method_hints.vi:12:58
   |
12 |     print(text.count(), text.count("a", "b"), text.count(1))
   |                                                          ^ expected `str`, found `int`
   |
   = help: convert it with `str(...)`, or use an f-string
```

**What to do.** The message shows the type expected and the type found. Convert (`str(x)`, `float(x)`, `int(x)`), or change the declaration. Numbers do not convert implicitly, and a value that may be `None` needs `.unwrap()`, `?`, a `match`, or a check first.

### E0301: wrong arg count

A call with the wrong number of arguments.

From `tests/ui/builtin_method_hints.vi`:

```text
error[E0301]: `str.count` takes 1 argument but 0 were given
  --> tests/ui/builtin_method_hints.vi:12:11
   |
12 |     print(text.count(), text.count("a", "b"), text.count(1))
   |           ^^^^^^^^^^^^ expected 1 argument
```

**What to do.** Pass exactly the parameters the function declares, in order or by name, leaving out only those that have defaults. The label shows where the function is defined.

### E0302: return value from none

A function without `->` returns a value.

From `tests/ui/return_value_without_type.vi`:

```text
error[E0302]: function `double` has no return type but returns a value
 --> tests/ui/return_value_without_type.vi:2:12
  |
2 |     return n * 2
  |            ^^^^^ returns `int`
  |
  = help: declare the return type in the signature: `-> int`
```

**What to do.** A function with no `->` returns nothing. Add the return type, or remove the `return` with a value.

### E0303: missing return

A function with a return type can reach its end without returning.

From `tests/ui/missing_return.vi`:

```text
error[E0303]: function `answer` can finish without returning a value
 --> tests/ui/missing_return.vi:1:5
  |
1 | def answer() -> int:
  |     ^^^^^^ can reach its end without `return`
  |                 --- declared to return `int`
  |
  = help: add a `return` statement at the end of the function
```

**What to do.** Every path must end in a `return` when the function has a return type, including the `else` of an `if`. Add one at the end, or restructure so no path falls off.

### E0304: unknown type

A type hint names a type that does not exist.

From `tests/ui/unknown_type.vi`:

```text
error[E0304]: unknown type `string`
 --> tests/ui/unknown_type.vi:1:21
  |
1 | def test(a: int) -> string:
  |                     ^^^^^^ not a type
  |
  = help: the string type is called `str`, as in Python
```

**What to do.** Check the spelling (the help suggests a type) and that the type is defined or imported. Built-in types are lower case (`str`, `list[int]`).

### E0305: missing type hint

A parameter without a type hint.

From `tests/ui/missing_type_hint.vi`:

```text
error[E0305]: parameter `w` has no type hint
 --> tests/ui/missing_type_hint.vi:1:10
  |
1 | def area(w, h: int) -> int:
  |          ^ needs a type hint
  |
  = help: write the type after the name, for example `w: int`
```

**What to do.** Every parameter needs a type hint: `def f(x: int)`. The language infers types of variables, not of parameters.

### E0306: bad main signature

`main` has parameters or a return type.

From `tests/ui/definition_errors.vi`:

```text
error[E0306]: `main` cannot take parameters
  --> tests/ui/definition_errors.vi:12:5
   |
12 | def main(args: list[str]) -> int:
   |     ^^^^ defined with parameters
```

**What to do.** `def main():` takes nothing and returns nothing. Read arguments with `os.args()` and signal failure with `os.exit(code)`.

### E0307: redeclared variable

A type hint on an assignment to a variable that already exists.

From `tests/ui/definition_errors.vi`:

```text
error[E0307]: `count` already has a type
  --> tests/ui/definition_errors.vi:14:12
   |
13 |     count: int = 1
   |     ----- `count` was declared as `int` here
14 |     count: int = 2
   |            ^^^ a type hint is only allowed on the first assignment
   |
   = help: remove the type hint
```

**What to do.** Give the type once, where the variable is first assigned, and assign later without a hint.

### E0308: none variable

A variable whose value has type `None`, which carries no information.

From `tests/ui/definition_errors.vi`:

```text
error[E0308]: `nothing` would have type `None`
  --> tests/ui/definition_errors.vi:15:15
   |
15 |     nothing = None
   |               ^^^^ the type cannot be inferred from `None`
   |
   = help: give it an optional type: `nothing: int? = None`
```

**What to do.** A variable of type `None` has no use. Give it a real type with a hint, such as `value: int? = None`, to hold a value or nothing.

### E0309: unexpected type args

Type arguments on a type that takes none.

From `tests/ui/generic_inference.vi`:

```text
error[E0309]: `Stack` takes 1 type argument
  --> tests/ui/generic_inference.vi:16:8
   |
16 |     t: Stack[int, str] = Stack(items=[1])
   |        ^^^^^^^^^^^^^^^ expected 1 type argument
   |
   = help: for example `Stack[int]`
```

**What to do.** Remove the `[...]`; the type does not take type arguments, or the type argument count is wrong. `Stack[int]` needs one for `struct Stack[T]`.

### E0310: none parameter

A parameter whose type is `None`.

From `tests/ui/definition_errors.vi`:

```text
error[E0310]: parameter `x` cannot have type `None`
 --> tests/ui/definition_errors.vi:9:19
  |
9 | def takes_none(x: None):
  |                   ^^^^ a `None` parameter carries no information
```

**What to do.** A parameter holding `None` carries nothing. Remove it, or make it optional with `T?`.

### E0311: missing field

A struct construction that leaves out a field without a default.

From `tests/ui/missing_struct_field.vi`:

```text
error[E0311]: missing field `id` in `User(...)`
 --> tests/ui/missing_struct_field.vi:7:12
  |
1 | struct User:
  |        ---- `User` is defined here
  ...
7 |     user = User(name="Ada")
  |            ^^^^^^^^^^^^^^^^ missing `id`
```

**What to do.** Give the missing field, or a default in the struct (`field: int = 0`).

### E0312: const not literal

A constant whose value cannot be worked out when compiling, or is defined in terms of itself.

From `tests/ui/const_errors.vi`:

```text
error[E0312]: the value of constant `CALLED` cannot be worked out when compiling
 --> tests/ui/const_errors.vi:3:16
  |
3 | const CALLED = len("abc")
  |                ^^^^^^^^^^ a call
  |
  = help: constants are worked out when compiling, so they cannot call functions
```

**What to do.** A constant is a number, string, `True`/`False`, or an expression of those, other constants and operators; it is worked out when compiling. Move anything else (calls, collections, f-strings) into a function.

### E0313: unhashable key

A dict key type that cannot be hashed.

From `tests/ui/set_errors.vi`:

```text
error[E0313]: `list[int]` cannot be used as a dict key
  --> tests/ui/set_errors.vi:11:16
   |
11 |     _nested = {[1], [2]}
   |                ^^^ not hashable
   |
   = help: dict keys can be `int`, `str`, `bool`, tuples of those, or types that implement `Hash`
```

**What to do.** Dict keys and set elements must be hashable: `int`, `str`, `bool`, tuples of them, and types with `@derive(Eq, Hash)`. A list, dict or set cannot be one; use a tuple, or a string made from it.

### E0314: bad format spec

An f-string format spec that is malformed or does not suit the value.

From `tests/ui/bad_format_spec.vi`:

```text
error[E0314]: invalid format spec `.2f`
 --> tests/ui/bad_format_spec.vi:3:19
  |
3 |     print(f"{name:.2f}")
  |                   ^^^ format type `f` cannot be used with `str`
  |
  = help: the spec follows Python's format mini-language, for example `.2f`, `>8` or `,`
```

**What to do.** The spec follows Python's format mini-language: `[fill][align][sign][#][0][width][,][.precision][type]`. The type letter must suit the value: `d`, `x`, `b` for `int`, `f`, `e`, `g`, `%` for `float`, `s` for `str`.

### E0315: cannot infer

A type that cannot be inferred, such as that of `[]` with no type hint.

From `tests/ui/closure_errors.vi`:

```text
error[E0315]: cannot infer the types of this lambda's parameters
 --> tests/ui/closure_errors.vi:5:14
  |
5 |     double = lambda x: x * 2
  |              ^^^^^^^^^^^^^^^ parameter types unknown
  |
  = help: use a lambda where a function type is expected, such as an argument, or define the function with `def` and type hints
```

**What to do.** The compiler could not tell the type. Add a type hint where the value is stored (`items: list[int] = []`), or pass an argument that has the type.

### E0316: no such member

A field or method that does not exist.

From `tests/ui/builtin_method_hints.vi`:

```text
error[E0316]: `str` has no method `splitlines`
  --> tests/ui/builtin_method_hints.vi:10:16
   |
10 |     print(text.splitlines(), text.isdigit(), text.isalpha(), text.removeprefix("a"))
   |                ^^^^^^^^^^ no such method
   |
   = help: the method is called `split_lines`
```

**What to do.** Check the spelling (the help suggests the nearest name) and the type of the value: it may be an optional (`.unwrap()` or test for `None` first), or a Python method the language spells differently (`starts_with`).

### E0317: not bool

A condition that is not a `bool`.

From `tests/ui/truthiness.vi`:

```text
error[E0317]: a condition must be a `bool`, found `int`
 --> tests/ui/truthiness.vi:3:8
  |
3 |     if len(items):
  |        ^^^^^^^^^^ has type `int`
  |
  = help: values are not true or false by themselves; compare explicitly, for example `len(items) > 0`
```

**What to do.** Conditions must be `bool`: there is no truthiness. Write `len(items) > 0`, `x is not None`, `text != ""`.

### E0318: not iterable

A `for` loop over something that cannot be iterated.

From `tests/ui/set_errors.vi`:

```text
error[E0318]: cannot make a set from a `int`
  --> tests/ui/set_errors.vi:25:15
   |
25 |     print(set(5), set(1, 2), set([1]) < set([2]), items + items, items <= items)
   |               ^ has type `int`
   |
   = help: `set(...)` takes a list, a string, a set, a dict (its keys) or `range(...)`
```

**What to do.** Loop over a list, string, dict, set, `range(...)`, channel, or a type with `iter` or `next` methods. To loop over a dict's entries, use `d.items()`.

### E0319: unpack mismatch

Unpacking a value into the wrong number of names.

From `tests/ui/unpack_mismatch.vi`:

```text
error[E0319]: cannot unpack `tuple[int, int, int]` into 2 names
 --> tests/ui/unpack_mismatch.vi:2:5
  |
2 |     a, b = (1, 2, 3)
  |     ^^^^ the value is `tuple[int, int, int]`
  |
  = help: unpacking needs a tuple with exactly as many elements as names
```

**What to do.** The name list and the value must have the same length. Unpack a tuple of two into two names.

### E0320: bad index

Indexing something that cannot be indexed that way.

From `tests/ui/del_errors.vi`:

```text
error[E0320]: `del` cannot remove an element of `int`
 --> tests/ui/del_errors.vi:9:9
  |
9 |     del n[0]
  |         ^^^^ has type `int`
  |
  = help: only lists and dicts have elements to remove
```

**What to do.** Only lists and dicts are indexed by `[...]`, by an `int` and by a key. A string is sliced with byte offsets (`s[a:b]`); a set has no positions; a tuple's index must be a literal.

### E0321: bad operands

An operator applied to types it does not support.

From `tests/ui/extern_and_with.vi`:

```text
error[E0321]: `//` is not available on `i32`
  --> tests/ui/extern_and_with.vi:20:17
   |
20 |     print(tiny, i32(1) // i32(2))
   |                 ^^^^^^^^^^^^^^^^ both sides are `i32`
   |
   = help: convert first: `int(a) // int(b)`
```

**What to do.** Both sides of an operator need compatible types: convert with `float(x)` or `int(x)`, or use the operation that exists (`a | b` for sets). Comparing structs needs `@derive(Eq)` or `@derive(Ord)`.

### E0322: unused result

A `Result` that is neither used nor explicitly discarded.

From `tests/ui/unused_result.vi`:

```text
error[E0322]: this `Result` is not used
 --> tests/ui/unused_result.vi:5:5
  |
5 |     load("config.toml")
  |     ^^^^^^^^^^^^^^^^^^^ may hold an error that is being ignored
  |
  = help: pass the error on with `?`, handle it with `match`, or discard it with `_ = ...`
```

**What to do.** A `Result` may hold an error. Pass it on with `?`, handle it with `match`, take the value with `.unwrap()`, or discard it deliberately with `_ = call()`.

### E0323: frozen field

Assigning to a field of a `@frozen` struct.

From `tests/ui/frozen_field.vi`:

```text
error[E0323]: cannot change a field of `Config`, which is `@frozen`
 --> tests/ui/frozen_field.vi:7:12
  |
2 | struct Config:
  |        ------ `Config` is declared `@frozen` here
  ...
7 |     config.port += 1
  |            ^^^^ frozen field
  |
  = help: build a new value instead, or remove `@frozen` if the struct should be mutable
```

**What to do.** A `@frozen` struct cannot be changed after it is made. Build a new one with the changed field, or remove `@frozen`.

### E0324: missing required trait

An implementation whose trait requires another trait the type does not implement.

From `tests/ui/derive_requires_eq.vi`:

```text
error[E0324]: `Key` implements `Hash` but not `Eq`, which `Hash` requires
 --> tests/ui/derive_requires_eq.vi:1:9
  |
1 | @derive(Hash)
  |         ^^^^ `Hash` requires `Eq`
  |
  = help: add `Eq` to the `@derive(...)` list
```

**What to do.** A trait can require others: `trait B(A)`. Implement the required trait for the type first.

### E0325: not object safe

A trait used as a type although some method prevents it, or a trait method with type parameters.

From `tests/ui/generic_method_errors.vi`:

```text
error[E0325]: trait method `pick` cannot have type parameters
 --> tests/ui/generic_method_errors.vi:4:14
  |
4 |     def pick[T](self, first: T, second: T) -> T
  |              ^ not allowed in a trait
  |
  = help: a trait can be used as a type, which needs every method to be one function; write a generic function, or give the type parameter to the trait's implementations
```

**What to do.** A trait used as a type must have only methods that take `self` and no type parameters of their own, and `Self` only as the receiver. Make the method a function, or avoid using the trait as a type.

### E0326: non exhaustive

A `match` that does not cover every possible value.

From `tests/ui/match_expression_errors.vi`:

```text
error[E0326]: this `match` does not cover every case
 --> tests/ui/match_expression_errors.vi:9:12
  |
9 |     return match c:
  |            ^^^^^^^^ not covered: Blue
  |
  = help: add the missing cases, or `case _:` to cover everything else
```

**What to do.** Add the cases the label lists, or `case _:` to cover the rest. A `match` on a trait object always needs `case _:`.

### E0327: bad try

`?` in a function whose return type cannot carry the error or `None`.

From `tests/ui/question_mark_outside_result.vi`:

```text
error[E0327]: `?` passes an error to the caller, but this function does not return a `Result`
 --> tests/ui/question_mark_outside_result.vi:2:12
  |
2 |     port = int.parse("8080")?
  |            ^^^^^^^^^^^^^^^^^^ error has nowhere to go
  |
  = help: declare the function as returning a `Result` (it returns `None`), or handle the error here with `match`
```

**What to do.** `?` returns the error (or `None`) to the caller, so the function must return a `Result` (or optional) that can hold it. Change the return type, or handle the value with `match`.

### E0328: bad impl

An implementation that is missing a method, has an extra one, or has the wrong signature.

From `tests/ui/impl_signature_mismatch.vi`:

```text
error[E0328]: `area` does not match its declaration in `Shape`
 --> tests/ui/impl_signature_mismatch.vi:8:9
  |
2 |     def area(self) -> float
  |         ---- declared here
  ...
8 |     def area(self) -> int:
  |         ^^^^ the return type should be `float`
```

**What to do.** An `impl Trait for Type` must define exactly the trait's methods, with the same parameters and return types (it may leave out those that have a default).

### E0329: bad pattern

A pattern that does not fit the value being matched.

From `tests/ui/trait_type_pattern_errors.vi`:

```text
error[E0329]: `Unrelated` does not implement `Shape`, so a `Shape` is never one
  --> tests/ui/trait_type_pattern_errors.vi:31:14
   |
31 |         case Unrelated(u):
   |              ^^^^^^^^^^^^ this case can never match
   |
   = help: implement `Shape` for `Unrelated`, or remove the case
```

**What to do.** A pattern must fit the type being matched: variants of that enum with the right number of fields, literals of the right type, tuples of the right length. On a trait object the pattern is a type that implements the trait.

### E0330: bad annotation

An annotation that does not exist or cannot be used here.

From `tests/ui/extern_and_with.vi`:

```text
error[E0330]: a value of type `list[int]` cannot be passed to a C function
 --> tests/ui/extern_and_with.vi:2:14
  |
2 | def c_abs(x: list[int]) -> dict[str, int]
  |              ^^^^^^^^^ not a C type
  |
  = help: `@extern` functions take numbers, `bool` and `str`
```

**What to do.** Annotations are a fixed set: `@derive`, `@frozen`, `@json`, `@test`, `@extern`, and `@http.get`, `@http.post` and the like. The message says which are allowed where.

### E0331: unsatisfied bound

A type argument that does not implement a trait its type parameter requires.

From `tests/ui/builtin_method_hints.vi`:

```text
error[E0331]: `Point` does not implement `Eq`, which `list.count` requires of `T`
  --> tests/ui/builtin_method_hints.vi:13:29
   |
13 |     print(items.count("a"), [Point(1)].count(Point(2)))
   |                             ^^^^^^^^^^^^^^^^^^^^^^^^^^ `T` is `Point` here
   |
   = help: implement `Eq` for `Point`, or derive it
```

**What to do.** The type argument must implement the trait the parameter's bound names. Derive or implement it (`@derive(Eq, Hash)`), or use a different type.

### E0332: assign captured

Assigning, from a nested function or lambda, to a variable of the enclosing function.

From `tests/ui/closure_errors.vi`:

```text
error[E0332]: cannot assign to `total`, which belongs to the enclosing function
 --> tests/ui/closure_errors.vi:8:9
  |
8 |         total = total + n
  |         ^^^^^ assigned inside a nested function
  |
  = help: a nested function can read the enclosing function's variables but not reassign them; keep changing state in a list or a struct field
```

**What to do.** A nested function or lambda can read the enclosing function's variables but not assign them. Keep changing state in a list, a struct field, or an `Atomic`, or return the new value.

### E0333: bad function value

A call to a function value with keyword arguments, or other misuse of a function value.

From `tests/ui/closure_errors.vi`:

```text
error[E0333]: a function value cannot take keyword arguments
  --> tests/ui/closure_errors.vi:13:13
   |
13 |     print(g(x=1))
   |             ^ keyword argument
   |
   = help: function types only know the types of their parameters, not their names
```

**What to do.** A function value's type has the types of its parameters but not their names, so it takes positional arguments only. Call it positionally, or define a `def` with the names and call that.

### E0334: unknown module

An import of a module that does not exist.

From `tests/ui/module_errors.vi`:

```text
error[E0334]: cannot find module `lib.nowhere`
 --> tests/ui/module_errors.vi:2:8
  |
2 | import lib.nowhere
  |        ^^^^^^^^^^^ no such module
  |
  = help: looked for `tests/ui/lib/nowhere.vi` and for a standard library module; the standard library has `io`, `os`, `time`, `json`, `log`, `http`, `re`, `crypto`, `auth`, `postgres`
```

**What to do.** Check the module name and that its file is in the project directory or a dependency. Standard modules are `io`, `os`, `json`, `http`, `re`, `time`, `log` and `postgres`.

### E0335: private

Using a declaration or member that is private to another file.

From `tests/ui/module_privacy.vi`:

```text
error[E0335]: `internal_helper` is private to module `lib.accounts`
 --> tests/ui/module_privacy.vi:2:35
  |
2 | from lib.accounts import Account, internal_helper, open_acount
  |                                   ^^^^^^^^^^^^^^^ not `pub`
  |
  = help: mark it `pub` in `lib.accounts` to use it from other files
```

**What to do.** Mark the declaration `pub` in its own file to use it from another, or use what the module offers instead. Private struct fields are only visible in the file that defines the struct.

### E0336: import cycle

Modules that import each other, directly or not.

From `tests/ui/module_errors.vi`:

```text
error[E0336]: modules import each other: lib.cycle_a -> lib.cycle_b -> lib.cycle_a
 --> tests/ui/lib/cycle_b.vi:1:8
  |
1 | import lib.cycle_a
  |        ^^^^^^^^^^^ this import closes the cycle
  |
  = help: move what both modules need into a third module that both import
```

**What to do.** Break the cycle: move what both files need into a third file, or pass a function value instead of importing.

### E0337: bad with

A `with` statement whose value has no `exit` method.

From `tests/ui/extern_and_with.vi`:

```text
error[E0337]: a value of type `int` cannot be used with `with`
  --> tests/ui/extern_and_with.vi:15:10
   |
15 |     with 5 as five:
   |          ^ has type `int`
   |
   = help: a value works with `with` when its type has a method `def exit(self)`, which runs when the block ends
```

**What to do.** `with value as name:` needs the value's type to have `def exit(self)` (and optionally `enter`). Files and mutex guards have them; a `Result` needs unwrapping first.

### E0338: not shareable

A value that would be handed to another task but can be changed, so the tasks would race.

From `tests/ui/lock_guard_escape.vi`:

```text
error[E0338]: a value of type `list[Item]` cannot be passed to another task
  --> tests/ui/lock_guard_escape.vi:18:20
   |
18 |         spawn keep(items)
   |                    ^^^^^ a `list[Item]` can be changed
   |
   = help: share an immutable `tuple[Item, ...]` made with `tuple(items)`, or wrap the list in `Mutex(...)`
```

**What to do.** Values given to a task, channel or mutex must not be changeable by two tasks. Share a `@frozen` struct, a tuple or `frozendict`, or put the changeable value in a `Mutex`.

### E0339: guard escape

A value protected by a mutex leaving the `with` block that holds the lock.

From `tests/ui/lock_guard_escape.vi`:

```text
error[E0339]: a value protected by a mutex cannot be stored in another variable
  --> tests/ui/lock_guard_escape.vi:16:17
   |
16 |         saved = items
   |                 ^^^^^ reached through `items`, which is only valid while the lock is held
   |
   = help: copy what you need while holding the lock, or keep the work inside the `with` block
```

**What to do.** The value protected by `with m.lock() as value:` is valid only inside the block. Copy out what you need (a number, a string, or a copy of a collection) before the block ends.

### E0340: bad route

An `@http` route whose path, parameters or return type do not fit together.

From `tests/ui/route_errors.vi`:

```text
error[E0340]: the path has `{id}`, but `missing_param` has no parameter `id`
  --> tests/ui/route_errors.vi:15:5
   |
15 |     @http.get("/notes/{id}")
   |     ^^^^^^^^^^^^^^^^^^^^^^^^ `{id}` is not used
```

**What to do.** Route paths start with `/` and name each `{parameter}` as a parameter of the handler; query and body parameters must have types that can be read from text or JSON. The message says which part does not fit.

### E0341: two types share a name in the API documentation

The OpenAPI document that `server.docs` and `server.openapi_json` serve lists each JSON type under `components.schemas` by its name, with a generic type's arguments joined by `_` (`Page[Todo]` is `Page_Todo`). Two different types that a server documents would get the same name. The name `Error` is taken by the body of error responses.

From `tests/ui/openapi_name_clash.vi`:

```text
error[E0341]: two schemas are both called `Box_Item` in the API documentation
  --> tests/ui/openapi_name_clash.vi:32:5
   |
32 |     server.mount(App())
   |     ^^^^^^^^^^^^^^^^^^^ documents `Box[Item]`
   |
   = help: `Box_Item` is documented under the same name; rename one of them
```

**What to do.** Rename one of the types. The check happens only in programs that call `docs` or `openapi_json`.

### E0342: a sign-in annotation or parameter that does not fit

`@auth.required` and parameters of type `auth.User` protect `@http` routes (docs/05, "Signing in with Google"). The error is `@auth.required` on a function that is not a route or with arguments, an `auth.User` parameter in a WebSocket handler, or two of them in one handler.

From `tests/ui/auth_route_errors.vi`:

```text
error[E0342]: `@auth.required` is on `helper`, which is not a route
 --> tests/ui/auth_route_errors.vi:5:1
  |
5 | @auth.required
  | ^^^^^^^^^^^^^^ no `@http` annotation
  |
  = help: `@auth.required` protects a route; for a call-style handler use `auth.protect(handler)`
```

**What to do.** Put `@auth.required` on a route, or wrap a call-style handler with `auth.protect(handler)`. To limit who may sign in, give `auth.google` `allowed_domains=` or `allowed_emails=`, or check the user in the handler. In a WebSocket handler, take an `http.Request` and call `auth.user(req)`. One `auth.User` parameter is enough.

## Warnings

### W0001: unused variable

A variable that is assigned but never read.

From `tests/ui/unused_variable.vi`:

```text
warning[W0001]: variable `unused` is assigned but never used
 --> tests/ui/unused_variable.vi:2:5
  |
2 |     unused = 1
  |     ^^^^^^ assigned here
  |
  = help: if this is intentional, start the name with an underscore: `_unused`
```

**What to do.** Use the variable, remove the assignment, or start the name with an underscore (`_unused`) to say it is intended.

### W0002: unreachable code

Statements after a `return`.

From `tests/ui/unreachable_code.vi`:

```text
warning[W0002]: unreachable code
 --> tests/ui/unreachable_code.vi:3:5
  |
3 |     print("never")
  |     ^^^^^^^^^^^^^^ this never runs
  |
  = help: it comes after a statement that always leaves this block
```

**What to do.** Remove the statements after `return`, or move the `return`.

### W0003: unreachable case

A `case` that can never match, because earlier cases cover everything it would.

From `tests/ui/unreachable_case.vi`:

```text
warning[W0003]: this case can never match
 --> tests/ui/unreachable_case.vi:5:14
  |
5 |         case 0:
  |              ^ covered by the cases above
  |
  = help: remove it, or move it before the case that covers it
```

**What to do.** An earlier case already matches everything this one would. Remove it, or move it before the case that covers it.

### W0004: unused import

An `import` of a name the file never uses.

From `tests/ui/module_privacy.vi`:

```text
warning[W0004]: `open_acount` is imported but never used
 --> tests/ui/module_privacy.vi:2:52
  |
2 | from lib.accounts import Account, internal_helper, open_acount
  |                                                    ^^^^^^^^^^^ imported here
  |
  = help: remove the import, or use it
```

**What to do.** Remove the import, or use it. For `from m import a, b` each name is checked on its own.
