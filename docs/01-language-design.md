# 01 — Language design

This document defines what Weby source looks like and what it means. The type system has its own document ([02](02-type-system-and-traits.md)). Each feature is tagged with the milestone that introduces it ([06](06-roadmap.md)).

Code blocks here are Weby. They are highlighted as Python because the surface syntax is close.

## Design rules

1. If Python has a readable way to write it and it can be compiled statically, use Python's spelling.
2. Where Python relies on dynamism (classes, exceptions, duck typing), use Rust's concept with Python's punctuation.
3. Types are written only where a reader needs them: function signatures and struct fields. Locals are inferred.
4. No feature that needs a runtime interpreter, runtime type lookup by name, or hidden control flow.

## Lexical structure (M1)

- Source files are UTF-8 with the extension `.wb`.
- Blocks are introduced by `:` and delimited by indentation. Indentation is spaces only; a tab in leading whitespace is a compile error. The amount is free but must be consistent within a block; the formatter will use four.
- A statement ends at the end of the line. Inside `()`, `[]` or `{}` newlines and indentation are ignored, so long calls and literals wrap freely. There is no backslash continuation.
- Comments start with `#`. A `"""docstring"""` as the first statement of a function, struct or trait is kept for tooling.
- Identifiers are `snake_case` for values and functions, `CamelCase` for types and traits. The compiler warns on violations; it does not use case for semantics.
- No semicolons. One statement per line.

Reserved words:

```
and as break case const continue def elif else enum False for from if impl
import in is lambda match None not or pass priv pub return self spawn struct
trait True while with
```

## Variables and constants (M1)

```python
count = 0                # declares count: int
name = "weby"            # declares name: str
ratio: float = 1         # annotation when the literal alone would infer something else
items: list[int] = []    # annotation required: an empty literal has no element type

count = count + 1        # reassignment; the type cannot change
count += 1
```

- First assignment declares. The scope is the enclosing function, as in Python, so a variable assigned in both arms of an `if` is usable afterwards.
- The compiler rejects any read that is not preceded by an assignment on every path.
- A variable has one type for its whole life.
- Assigned-but-never-read variables produce a warning.
- Module-level bindings must be `const` and must have a compile-time constant value in the prototype:

```python
const MAX_BODY_BYTES = 1_048_576
const SERVICE_NAME = "users"
```

There are no mutable globals. Shared state is created in `main` and passed down, or held in a `Mutex`.

## Built-in types

Weby includes Python's built-in types under Python's names, written the way Python type hints write them. User-defined types are structs, enums and traits.

| Python type | In Weby | Notes | Milestone |
|---|---|---|---|
| `int` | `int` | 64-bit signed. Python's `int` is unbounded; Weby's wraps around on overflow, as Go's does | M1 |
| `float` | `float` | 64-bit IEEE, the same as Python | M1 |
| `bool` | `bool` | `True` and `False`. Not a kind of `int`: `True + 1` is a type error | M1 |
| `str` | `str` | Immutable, UTF-8 | M1 |
| `None` | `None` | The type of "no value". `-> None` is a function that returns nothing | M1 |
| `range` | `range` | Iterable; a `for` over it compiles to a counting loop | M1 |
| `list[T]` | `list[T]` | Growable array. Every element has the same type `T` | M2 |
| `dict[K, V]` | `dict[K, V]` | Hash map, insertion ordered as in Python | M2 |
| `tuple[A, B]` | `tuple[A, B]` | Fixed size, immutable, written `(a, b)`. The variable-length form `tuple[int, ...]` is not supported; use a list | M2 |
| `set[T]` | `set[T]` | Hash set | M2 |
| `bytes` | `bytes` | Immutable byte string | M2 |
| `bytearray` | `bytearray` | Growable byte buffer | M5 |
| `frozenset[T]` | deferred | Post-prototype | |
| `complex` | deferred | No use in web backends; can be added later as a library struct | |
| `memoryview` | not planned | Slices of `bytes` already share the underlying data | |
| `object`, `type`, `Any` | not available | Every value has exactly one static type | |

Types Weby adds:

| Type | Meaning | Python equivalent |
|---|---|---|
| `T?` | Either a `T` or `None`. The Python spelling `T \| None` is accepted and means exactly the same | `T \| None`, `Optional[T]` |
| `Result[T]`, `Result[T, E]` | A `T` or an error | (exceptions) |
| `def(A, B) -> R` | Function value | `Callable[[A, B], R]` |
| `i8 i16 i32 u8 u16 u32 u64 f32` | Sized numerics for binary protocols and FFI (M5) | (none) |
| Structs, enums, traits | User-defined types | classes |

Where Weby differs from Python:

- **Collections are homogeneous.** `[1, "a"]` is a type error. Mixed data uses an enum, a trait as the element type, or `json.Value`.
- **`int` has a fixed size.**
- **`bool` is not an `int`.**
- **No truthiness.** Conditions must be `bool`, so `if items:` is an error and `if len(items) > 0:` is required.
- **General unions do not exist.** `int | str` is an error with a suggestion to use an enum; only `T | None` is accepted.

Familiar wrong spellings get a targeted error with the fix: `string` (use `str`), `integer`, `boolean`, `List[int]` and `Dict[...]` (use lower case), `Optional[T]` (use `T?`).

Operators follow Python: `+ - * / // % **`, `and or not`, `in`, `is`. `/` on two `int`s yields a `float`; `//` is integer division. Comparison chains are **not** supported (`a < b < c` is an error with a suggestion).

Both sides of an arithmetic operator must have the same type. `1 + 2.5` works because the literal `1` adapts to `float`; adding an `int` variable to a `float` variable is a compile error that suggests `float(x)`. The operators are defined by built-in traits (`AddNumber` and its siblings), described in [02](02-type-system-and-traits.md#arithmetic-traits).

## Strings (M1–M2)

```python
greeting = "hello"
path = 'single quotes are the same'
block = """
    triple-quoted strings span lines
"""
message = f"{user.name} has {len(items)} items, total {total:.2f}"
```

- `str` is immutable. `len(s)` is the byte length; iterating yields characters (Unicode scalar values). Indexing by integer is not provided, because it is either slow or wrong for UTF-8; slicing by byte offsets (`s[a:b]`) panics if it splits a character.
- f-strings (M2) accept any expression whose type implements `Display`, with an optional format spec after `:`.
- Common methods: `upper`, `lower`, `strip`, `split`, `join`, `starts_with`, `ends_with`, `find`, `replace`, `contains`.

## Control flow (M1, `match` in M3)

```python
if score >= 90:
    grade = "A"
elif score >= 80:
    grade = "B"
else:
    grade = "C"

while retries > 0:
    retries -= 1

for i in range(10):
    print(i)

for user in users:
    if not user.active:
        continue
    send(user)

for key, value in headers.items():
    print(f"{key}: {value}")

label = "even" if n % 2 == 0 else "odd"
```

`for` works on anything that implements `Iterable[T]` ([02](02-type-system-and-traits.md#built-in-traits)). `range` is compiled to a plain counting loop.

`pass` does nothing. It fills a block that needs no statements, such as an `impl` that only uses a trait's default methods.

## Functions (M1)

```python
def add(a: int, b: int) -> int:
    return a + b

def log(message: str):          # no return type means it returns nothing
    print(message)

def connect(host: str, port: int = 5432, timeout: float = 5.0) -> Result[Conn]:
    ...

conn = connect("db.internal", timeout=1.0)?
```

- Type hints are Python's: `name: type` on parameters, `-> type` for the return.
- Every parameter must have a hint. A parameter without one is a compile error.
- A function with no `->` returns `None`; `-> None` may also be written out. Returning a value from such a function is a compile error, so no function is ever untyped.
- Default values and keyword arguments for functions arrive in M4. Before that, function calls are positional. Struct construction accepts keywords from M2.
- No `*args` or `**kwargs`.
- Functions may be defined in any order; a function can call one defined later in the file.
- Program entry is `def main()`. Top-level statements other than declarations are not allowed.

## Structs and methods (M2)

```python
struct Point:
    x: float
    y: float

struct User:
    id: int
    name: str
    email: str? = None           # field default
    tags: list[str] = []         # evaluated per construction, not shared

impl User:
    def new(name: str) -> User:              # no self: called as User.new("ada")
        return User(id=next_id(), name=name)

    def display_name(self) -> str:
        return self.name.upper()

    def rename(self, name: str):
        self.name = name

p = Point(1.0, 2.0)              # positional, in field order
u = User(id=1, name="ada")       # keyword
u.rename("Ada")
```

- A struct has fields only. Behaviour lives in `impl` blocks, which may appear anywhere in the module that defines the struct.
- Struct values are references. `b = a` makes `b` refer to the same object. `a.copy()` makes a shallow copy when the struct derives `Copy`.
- Fields are mutable, unless the struct is marked `@frozen`, in which case no field can be reassigned after construction. Frozen structs are what can be shared freely between tasks.
- `self` is written explicitly as the first parameter, as in Python, and has no type annotation.
- No inheritance, no constructors beyond the generated one, no destructors.
- `==` on structs requires `Eq`; `is` compares identity.

## Traits (M3)

```python
trait Shape:
    def area(self) -> float
    def name(self) -> str

    def describe(self) -> str:                    # default method
        return f"{self.name()} with area {self.area():.1f}"

struct Circle:
    radius: float

impl Shape for Circle:
    def area(self) -> float:
        return 3.14159 * self.radius ** 2

    def name(self) -> str:
        return "circle"

def total_area(shapes: list[Shape]) -> float:     # dynamic dispatch
    total = 0.0
    for s in shapes:
        total += s.area()
    return total

def largest[T: Shape](items: list[T]) -> T?:      # static dispatch (M4)
    ...
```

A method signature with no body is required; one with a body is a default. Full rules are in [02](02-type-system-and-traits.md#traits).

## Enums and `match` (M3)

```python
enum Message:
    Text(body: str)
    Binary(data: bytes)
    Ping
    Close(code: int, reason: str)

def handle(msg: Message) -> str:
    match msg:
        case Text(body):
            return body
        case Binary(data):
            return f"{len(data)} bytes"
        case Close(code, _):
            return f"closed: {code}"
        case _:
            return ""
```

- Enums are tagged unions. Variants may carry named fields.
- `match` must be exhaustive; `case _:` is the catch-all.
- Patterns: variant with sub-patterns, literal, tuple, binding name, `_`, alternatives with `|`, and a guard (`case Text(body) if len(body) > 0:`).
- `match` is a statement in the prototype. Expression form is deferred.
- A variant is written `Message.Text("hi")`, or just `Text("hi")` when no other enum has a variant of that name. Variants without fields are values: `Message.Ping` or `Ping`. Fields can be given by position or by name, as for structs.
- In a pattern, a bare name that is a variant of the matched enum is that variant; any other bare name binds the value. Patterns may also name the enum: `case Message.Close(code, _):`.
- Enum values are immutable. Equality, hashing and printing come from `@derive(Eq, Hash, Display)`.

## Options (M2–M3)

```python
def find(users: list[User], id: int) -> User?:
    for u in users:
        if u.id == id:
            return u            # implicitly Some(u)
    return None

match find(users, 7):
    case Some(u):
        print(u.name)
    case None:
        print("missing")

email = user.email.unwrap_or("none")
```

- `T?` is sugar for `Option[T]`, an ordinary enum with `Some(T)` and `None`.
- A `T` is accepted where `T?` is expected and is wrapped automatically.
- There is no null. A `User` is never `None`; only a `User?` can be.
- Flow narrowing (`if user is not None:` makes `user` a `User` inside the block) is planned for M4. Until then, use `match`, `unwrap_or` and `?`.

## Errors

(M3)

```python
trait Error:
    def message(self) -> str

struct NotFound:
    what: str

impl Error for NotFound:
    def message(self) -> str:
        return f"{self.what} not found"

def load_user(id: int) -> Result[User]:
    row = db.query_one("select * from users where id = ?", id)?    # propagate on error
    if row is None:
        return Err(NotFound("user"))
    return parse_user(row)?         # implicitly Ok(...)
```

- `Result[T, E]` is an enum with `Ok(T)` and `Err(E)`. `Result[T]` means `Result[T, Error]`, where `Error` is the built-in trait used as a type.
- `expr?` evaluates to the `Ok` value, or returns the `Err` from the enclosing function. On `T?` it returns `None`. The enclosing function must return a compatible type.
- A value of type `T` is wrapped as `Ok` where a `Result[T]` is expected, so the happy path has no `Ok(...)` noise.
- A concrete error is converted to the `Error` trait object automatically at `?` and at `return Err(...)`.
- Unused `Result` values are a compile error. Discard explicitly with `_ = f()`.
- A function returning `Result[None]` succeeds by reaching its end or with a bare `return`, as a Python function does; only errors need writing out.
- `int.parse(text)` and `float.parse(text)` return `Result[int, ParseError]` and `Result[float, ParseError]`, accepting what Python's `int()` and `float()` accept.
- **Panics** are for bugs: index out of range, `unwrap` on `None`, integer division by zero, explicit `panic("...")`. They are not caught by user code. What a panic does to the process is defined in [04](04-runtime-and-memory.md#panics).

## Scoped cleanup with `with` (M5)

Memory is collected automatically, at no particular time. Anything that must be released promptly (files, locks, connections) uses `with`:

```python
with open("data.csv") as f:
    for line in f.lines():
        process(line)
# f is closed here, including on early return and on ?
```

`with` works on any type that implements the `Scoped[T]` trait, which has two methods: `enter(self) -> T` produces the value bound by `as`, and `exit(self)` runs on every way out of the block. A file's `enter` returns the file itself; a mutex guard's `enter` returns the protected value. There is no `defer` and there are no destructors.

## Closures (M4)

```python
doubled = map_list(numbers, lambda x: x * 2)   # parameter types come from `map_list`
names = sorted(users, key=lambda u: u.name)

def make_counter() -> def() -> int:
    count = [0]
    def next() -> int:
        count[0] += 1                    # captured by reference; the list is shared
        return count[0]
    return next
```

A function value is always a pair of code pointer and environment, so plain functions, methods bound to an object (`user.rename`), `Type.method` (taking the receiver first), built-in functions such as `len`, and closures are interchangeable where a `def(...) -> ...` type is expected.

A closure reads the enclosing function's variables as they are when it runs, but cannot assign to them (D35): `count = count + 1` inside `next` would be an error, so changing state lives in a list or a struct field. A lambda's parameter types come from the function type expected where it is written; `double = lambda x: x * 2` on its own line has no such type and is an error that suggests `def`.

Comprehensions build lists and dicts: `[f(x) for x in items if x > 0]`, `{k: v for k, v in pairs}`. Their loop variables belong to the comprehension.

Parameters can have defaults, and calls can name arguments: `def greet(name: str, greeting: str = "Hello")`, `greet("Ada", greeting="Hi")`. A default is evaluated afresh at each call that leaves it out.

## Concurrency (M6)

```python
def fetch_all(urls: list[str]) -> list[str]:
    tasks = [spawn fetch(url) for url in urls]
    return [t.join() for t in tasks]

def worker(jobs: Channel[Job], results: Channel[Outcome]):
    for job in jobs:                     # ends when the channel is closed
        results.send(run(job))

stats = Mutex(Stats(hits=0, errors=0))
with stats.lock() as s:                  # s is the Stats; unlocked when the block ends
    s.hits += 1

requests = Atomic(0)                     # for plain counters, no lock needed
requests.increment()
```

- `spawn call(...)` starts a task and returns a `Task[T]`. `join()` waits and returns the result.
- Tasks communicate through `Channel[T]` or share state through `Mutex[T]`.
- The compiler checks what crosses between tasks. Arguments to `spawn` and values sent on a channel must be immutable (scalars, strings, `@frozen` structs such as `Job` and `Outcome` above) or synchronised (`Mutex`, `Atomic`, `Channel`). Handing an ordinary mutable struct or list to another task is a compile error. The rules are in [04](04-runtime-and-memory.md#data-race-prevention).
- There is no `async` and no `await`. Reading a socket inside a task looks like a blocking call; the runtime decides what actually blocks. See [04](04-runtime-and-memory.md#concurrency).
- List comprehensions (`[f(x) for x in xs if cond]`) arrive in M4.

## Modules

(M5; not required for the first vertical slice)

```python
# models.wb
pub struct User:
    id: int
    name: str
    priv password_hash: str              # hidden outside models.wb

impl User:
    def check_password(self, attempt: str) -> bool:      # visible wherever User is
        return hash_password(attempt) == self.password_hash

    priv def rehash(self):               # hidden outside models.wb
        ...

pub def load_user(id: int) -> Result[User]:
    ...

def hash_password(text: str) -> str:     # no `pub`: private to models.wb
    ...
```

```python
# main.wb
import http
import models                            # models.wb next to this file
from models import User, load_user
import services.billing as billing       # services/billing.wb
```

- One file is one module. A directory groups modules; there is no `__init__` file.
- Lookup order: the importing file's directory tree (project root), then the standard library.
- Import cycles are an error in the prototype.
- Importing never executes code, because modules contain only declarations and constants.

### Visibility

Two rules, one for each keyword:

1. **`pub` exports a declaration from its file.** A top-level `def`, `struct`, `enum`, `trait` or `const` is private to the file that defines it unless it is marked `pub`.
2. **`priv` hides a member inside a type.** Fields, methods and enum variants are visible wherever their type is visible, unless marked `priv`, which restricts them to the file that defines the type.

Details:

- Either keyword may also be written where it is already the default (`priv def helper()`, `pub name: str`), for people who prefer everything spelled out.
- A struct with a `priv` field cannot be constructed directly outside its file, since the caller cannot supply the field. The file provides a `pub` function that builds it.
- A `pub` declaration may not mention a private type in its signature.
- Methods of a trait are as visible as the trait. `main` needs no `pub`.
- A single-file program never needs either keyword.
- Using a private name from another file is a compile error that says where the name is defined and that it is not `pub`.

## Annotations (M3 onward)

`@name(...)` before a declaration is a fixed, compiler-known annotation. Users cannot define their own; this is what keeps them cheap to compile.

| Annotation | Meaning | Milestone |
|---|---|---|
| `@derive(Eq, Hash, Display, Copy, Json)` | Generate the trait implementation from the fields | M3, `Json` in M7 |
| `@frozen` on a struct | No field can be reassigned after construction. Required for sharing a struct between tasks | M3; sharing checks in M6 |
| `@json(name="userId")` on a field | Rename in JSON | M7 |
| `@http.get(path)`, `@http.post`, `@http.put`, `@http.patch`, `@http.delete` on a function or method | Declare an HTTP route. Parameters are bound from the request and checked at compile time ([05](05-standard-library.md#http)) | M8 |
| `@http.ws(path)` | Declare a WebSocket endpoint | M9 |
| `@extern("c_symbol")` on a bodiless `def` | Bind a C function | M5 |
| `@test` | Mark a test function | post-prototype |

## Deliberately left out

| Python feature | Why it is absent | What to use |
|---|---|---|
| Classes and inheritance | The language's premise | Structs, traits, composition |
| Exceptions | Hidden control flow, unwinding cost | `Result` and `?` |
| Truthiness | Ambiguous under static types | Explicit comparisons |
| Dynamic attributes, `getattr`, monkey patching | Needs a runtime object model | Struct fields, `dict` |
| `*args`, `**kwargs` | Untyped | Lists, structs, default arguments |
| User-defined decorators | Need first-class runtime function rewriting | The fixed annotations, which include route annotations such as `@http.get` |
| Generators (`yield`) | Coroutine transform in the compiler | Iterator structs, channels |
| `global`, mutable module state | Data races, initialisation order | Pass state, `Mutex` |
| Multiple assignment targets with starred unpacking | Low value | Tuple unpacking only |

## Grammar sketch

Informal EBNF. `NEWLINE`, `INDENT` and `DEDENT` are produced by the lexer ([03](03-compiler-architecture.md#lexer)).

```
module      = { import | decl } EOF
import      = "import" path [ "as" NAME ] NEWLINE
            | "from" path "import" NAME { "," NAME } NEWLINE
decl        = { annotation } [ "pub" | "priv" ] ( func | struct | trait | enum | const )
            | impl

func        = "def" NAME [ generics ] "(" [ params ] ")" [ "->" type ] ( ":" block | NEWLINE )
struct      = "struct" NAME [ generics ] ":" INDENT { field NEWLINE } DEDENT
enum        = "enum" NAME [ generics ] ":" INDENT { variant NEWLINE } DEDENT
trait       = "trait" NAME [ generics ] [ "(" type { "," type } ")" ] ":" INDENT { func } DEDENT
impl        = "impl" [ generics ] [ type "for" ] type ":" INDENT { member } DEDENT
member      = { annotation } [ "pub" | "priv" ] func
const       = "const" NAME [ ":" type ] "=" expr NEWLINE

generics    = "[" NAME [ ":" bound ] { "," NAME [ ":" bound ] } "]"
bound       = type { "+" type }
params      = param { "," param }
param       = "self" | NAME ":" type [ "=" expr ]
field       = { annotation } [ "pub" | "priv" ] NAME ":" type [ "=" expr ]
variant     = NAME [ "(" field { "," field } ")" ]

type        = NAME { "." NAME } [ "[" type { "," type } "]" ] [ "?" ]
            | type "|" "None"
            | "def" "(" [ type { "," type } ] ")" [ "->" type ]

block       = NEWLINE INDENT stmt { stmt } DEDENT
stmt        = simple NEWLINE | if | while | for | match | with | func
simple      = expr | target [ ":" type ] assign_op expr
            | "return" [ expr ] | "break" | "continue"
match       = "match" expr ":" NEWLINE INDENT { "case" pattern [ "if" expr ] ":" block } DEDENT

expr        = ternary | lambda
```

Expression precedence, lowest to highest:

```
lambda
x if c else y
or
and
not
in, not in, is, is not, <, <=, >, >=, !=, ==
|   ^   &   <<  >>
+  -
*  /  //  %
unary -  +  ~
**
spawn
postfix: call ( ), index [ ], attribute ., propagate ?
```
