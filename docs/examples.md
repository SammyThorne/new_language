# Weby by example

This page teaches the language through short programs, from "hello" to a JSON web service. It assumes you can already program, preferably in Python: most of the syntax will look familiar, and the page points out where Weby differs.

Every program with a `def main():` on this page is compiled by the test suite. Where an output block follows a program, the test runs it and checks the output is exactly what is printed here. The server, database and network examples are only compiled, because they wait for connections. So everything below works as shown.

<!--
For maintainers: crates/lang_cli/tests/guide.rs reads this file. Each ```python block that
contains `def main():` is compiled; if the next block is a ```text block, the program is run and
its standard output must equal that block. ```python blocks without `def main():` (fragments,
module files, test files), and blocks right after a `<!-- not compiled ... -->` line, are not
compiled.
-->

**Contents**

1. [Running a program](#1-running-a-program)
2. [Values, variables and arithmetic](#2-values-variables-and-arithmetic)
3. [Control flow](#3-control-flow)
4. [Functions](#4-functions)
5. [Strings](#5-strings)
6. [Lists, dicts and tuples](#6-lists-dicts-and-tuples)
7. [Structs and methods](#7-structs-and-methods)
8. [Optional values](#8-optional-values)
9. [Enums and `match`](#9-enums-and-match)
10. [Errors and panics](#10-errors-and-panics)
11. [Traits](#11-traits)
12. [Generics](#12-generics)
13. [Iterators](#13-iterators)
14. [Modules and packages](#14-modules-and-packages)
15. [Files, the environment and `with`](#15-files-the-environment-and-with)
16. [Tasks, channels and shared state](#16-tasks-channels-and-shared-state)
17. [JSON](#17-json)
18. [A web service](#18-a-web-service)
19. [WebSockets](#19-websockets)
20. [The HTTP client](#20-the-http-client)
21. [PostgreSQL](#21-postgresql)
22. [Regular expressions, time and logging](#22-regular-expressions-time-and-logging)
23. [Writing tests](#23-writing-tests)
24. [Tools](#24-tools)
25. [Coming from Python](#25-coming-from-python)

## 1. Running a program

A program is a `.wb` file with a `main` function. Code outside functions is not allowed, apart from declarations, so the program always starts in `main`.

```python
def main():
    print("Hello, world!")
```

```text
Hello, world!
```

Save it as `hello.wb` and run it. `weby run` compiles to a temporary executable and runs it; `weby build` leaves the executable behind.

```bash
weby run hello.wb
```

```bash
weby build hello.wb --release -o hello
```

Debug builds compile faster and check more. `--release` turns on LLVM's optimisations, and is what you deploy and benchmark. Both produce one native executable with no runtime to install.

## 2. Values, variables and arithmetic

Variables are created by assigning to them. Their type is inferred from the first value, and a variable keeps that type: assigning a string to an `int` variable is a compile error. Write the type yourself with `name: type` where you want to, or where the value alone doesn't say (an empty list, for example).

```python
const VAT = 0.2

def main():
    count = 3
    price: float = 9.5
    name = "widget"
    in_stock = True
    print(count, price, name, in_stock)

    total = float(count) * price * (1.0 + VAT)
    print(f"{count} x {name} = {total:.2f}")

    print(7 / 2, 7 // 2, 7 % 2, -7 // 2, 2 ** 10)
    print(0.1 + 0.2, 1e16, abs(-4), round_down(3.9))

    biggest = 9223372036854775807
    print(biggest + 1)

    small: u8 = 250
    print(small + u8(10), i32(-3) * i32(4))

def round_down(x: float) -> int:
    return int(x)
```

```text
3 9.5 widget True
3 x widget = 34.20
3.5 3 1 -4 1024
0.30000000000000004 1e+16 4 3
-9223372036854775808
4 -12
```

Things to notice:

- `int` is a 64-bit integer that wraps around on overflow, as in Go and C, rather than growing without limit as in Python.
- `float` is a 64-bit float, printed exactly as Python prints it.
- An `int` and a `float` never mix silently. `float(count) * price` converts on purpose; `count * price` is a compile error that says so. An integer literal is accepted where a float is expected.
- `/` always gives a `float`; `//` and `%` round towards negative infinity, as in Python.
- The sized types `i8` to `i64`, `u8` to `u64`, `f32` and `f64` are for binary formats and C functions. They wrap at their own width.
- `const` declares a module-level constant.
- Functions can be used before they are defined.

## 3. Control flow

`if`, `elif`, `else`, `while`, `for`, `break`, `continue` and `pass` work as in Python. One difference: conditions must be `bool`. There is no truthiness, so `if items:` is an error. Write `if len(items) > 0:` instead.

```python
def classify(n: int) -> str:
    if n < 0:
        return "negative"
    elif n == 0:
        return "zero"
    return "small" if n < 10 else "large"

def main():
    for n in [-5, 0, 7, 42]:
        print(n, classify(n))

    countdown = ""
    for i in range(10, 0, -3):
        countdown = countdown + str(i) + " "
    print(countdown.strip())

    n = 27
    steps = 0
    while n != 1:
        n = n // 2 if n % 2 == 0 else 3 * n + 1
        steps += 1
    print("Collatz steps from 27:", steps)

    for word in ["apple", "", "cherry", "stop", "never"]:
        if word == "":
            continue
        if word == "stop":
            break
        print("word:", word)
```

```text
-5 negative
0 zero
7 small
42 large
10 7 4 1
Collatz steps from 27: 111
word: apple
word: cherry
```

The compiler also checks that every path through a function with a return type returns a value, that a variable is assigned on every path before it is read, and that no code follows a `return`.

A block that is a single simple statement can sit on the line of its header. `del` removes a list element or a dict entry, and `print` takes `sep` and `end`.

```python
def clamp(n: int, low: int, high: int) -> int:
    if n < low: return low
    elif n > high: return high
    else: return n

def main():
    print(clamp(-5, 0, 10), clamp(50, 0, 10), clamp(7, 0, 10))

    total = 0
    for n in range(1, 5): total += n
    print("total", total)

    items = [10, 20, 30, 40]
    del items[1]
    ages = {"ada": 36, "alan": 41}
    del ages["alan"]
    print(items, ages)

    print("a", "b", "c", sep="-")
    print("no newline", end="")
    print(" ... then one")
```

```text
0 10 7
total 10
[10, 30, 40] {'ada': 36}
a-b-c
no newline ... then one
```

Only a simple statement fits after the `:` (an expression, an assignment, `return`, `pass`, `break`, `continue`, `assert` or `del`); an `if` inside an `if` still needs its own indented block.

## 4. Functions

Parameters have types, and so does the return value unless the function returns nothing. Parameters can have defaults and can be passed by keyword. Functions are values: their type is written `def(int) -> int`.

```python
def greet(name: str, greeting: str = "Hello", punctuation: str = "!") -> str:
    return f"{greeting}, {name}{punctuation}"

def apply_twice(f: def(int) -> int, x: int) -> int:
    return f(f(x))

def make_adder(n: int) -> def(int) -> int:
    return lambda x: x + n

def factorial(n: int) -> int:
    def go(k: int, acc: int) -> int:
        if k <= 1:
            return acc
        return go(k - 1, acc * k)
    return go(n, 1)

def square(x: int) -> int:
    return x * x

def main():
    print(greet("Ada"))
    print(greet("Bob", "Hi"))
    print(greet("Cy", punctuation="?"))
    print(greet(greeting="Yo", name="Di"))

    add5 = make_adder(5)
    print(add5(1), apply_twice(add5, 0), apply_twice(square, 3))

    operations: dict[str, def(int) -> int] = {"double": lambda x: x * 2, "square": square}
    for name in ["double", "square"]:
        print(name, operations[name](7))

    print(factorial(10))
```

```text
Hello, Ada!
Hi, Bob!
Hello, Cy?
Yo, Di!
6 10 81
double 14
square 49
3628800
```

A `lambda` is a single expression. A nested `def` can hold statements. Both capture the variables around them.

## 5. Strings

Strings are immutable UTF-8 text. They have the methods you'd expect from Python, with `starts_with` and `ends_with` spelled with an underscore. f-strings take Python's format specifications.

```python
def main():
    text = "  Hello, World  "
    t = text.strip()
    print(t.upper(), t.lower(), len(t))
    print(t.find("World"), "World" in t, t.starts_with("Hello"), t.ends_with("!"))
    print(t[:5], t[-5:], t.replace("World", "there"))

    words = "the quick brown fox".split()
    print(words, "-".join(words))
    print("a,b,,c".split(","))

    letters: list[str] = []
    for ch in "héllo":
        letters.append(ch)
    print(letters)

    pi = 3.14159
    n = 1234567
    print(f"{pi:.2f}|{pi:8.3f}|{n:,}|{n:>10}|{42:05d}|{255:#x}|{0.25:.1%}|{'ab':*^6}")

    multi = """first line
second line"""
    print(multi.split("\n"))
    print(str(42) + "!", int.parse("17").unwrap() + 1)
```

```text
HELLO, WORLD hello, world 12
7 True True False
Hello World Hello, there
['the', 'quick', 'brown', 'fox'] the-quick-brown-fox
['a', 'b', '', 'c']
['h', 'é', 'l', 'l', 'o']
3.14|   3.142|1,234,567|   1234567|00042|0xff|25.0%|**ab**
['first line', 'second line']
42! 18
```

A string with an `r` in front is raw: backslashes stay as written, which suits Windows paths and patterns for `re`. In an f-string, `!r` shows a value as `repr` does, with a string's quotes, and a format spec after it pads that text.

```python
def main():
    path = r"C:\temp\new"
    print(path, len(path))
    name = "ada"
    print(f"{name!r} and {name!s} and {name!r:>7}|")
    print([1, 2], f"{[1, 2]!r}")
```

```text
C:\temp\new 11
'ada' and ada and   'ada'|
[1, 2] [1, 2]
```

The other string methods Python programmers reach for are there too. A name that Python runs together has an underscore here (`split_lines`, `is_digit`, `remove_prefix`), and the compiler suggests the right one if you write Python's.

```python
def main():
    line = "key = value = more"
    print(line.partition(" = "), line.rpartition(" = "))
    print(line.split(" = ", 1), line.rsplit(" = ", 1))
    print("banana".count("an"), "banana".rfind("an"), "Content-Type".lower().remove_prefix("content-"))
    print("*** hi ***".strip("* "), "42".is_digit(), "4x".is_digit(), "007".zfill(5), "-7".zfill(4))
    print("hello world".title(), "hELLO".capitalize(), "a\r\nb\n".split_lines())
    print(ord("A"), chr(97), [chr(ord("a") + i) for i in range(3)])
```

```text
('key', ' = ', 'value = more') ('key = value', ' = ', 'more')
['key', 'value = more'] ['key = value', 'more']
2 3 type
hi True False 00007 -007
Hello World Hello ['a', 'b']
65 a ['a', 'b', 'c']
```

Indexing and slicing count characters, not bytes. For binary data there is a separate `bytes` type: `"héllo".encode()` gives one, and `data.decode()` turns it back into text if it is valid UTF-8.

## 6. Lists, dicts and tuples

`list[T]`, `dict[K, V]` and tuples work as in Python, but every element of a list has the same type. A dict keeps keys in insertion order.

```python
def main():
    numbers = [3, 1, 4, 1, 5, 9, 2, 6]
    numbers.append(5)
    print(len(numbers), numbers[0], numbers[-1], numbers[2:5])
    print(sorted(numbers), sorted(numbers, reverse=True)[:3])
    print(sum(numbers), min(numbers), max(numbers), 9 in numbers)

    squares = [n * n for n in range(6)]
    evens = [n for n in numbers if n % 2 == 0]
    print(squares, evens)

    ages = {"ann": 31, "bob": 27}
    ages["cy"] = 45
    print(ages, ages["bob"], "dee" in ages)
    print(ages.get("dee"), ages.get_or("dee", 0))
    for name, age in ages.items():
        print(f"{name} is {age}")
    older = {name: age + 1 for name, age in ages.items()}
    print(older)

    point = (3, 4)
    x, y = point
    print(point, x + y)
    a = 1
    b = 2
    a, b = b, a
    print(a, b)

    for i, word in enumerate(["zero", "one"]):
        print(i, word)
    print(zip([1, 2, 3], ["a", "b", "c"]))

    words = ["pear", "Fig", "apple", "date"]
    print(sorted(words, key=lambda w: w.lower()), sorted(words, key=len))
    print(any([n > 8 for n in numbers]), all([n > 0 for n in numbers]))
```

```text
9 3 5 [4, 1, 5]
[1, 1, 2, 3, 4, 5, 5, 6, 9] [9, 6, 5]
36 1 9 True
[0, 1, 4, 9, 16, 25] [4, 2, 6]
{'ann': 31, 'bob': 27, 'cy': 45} 27 False
None 0
ann is 31
bob is 27
cy is 45
{'ann': 32, 'bob': 28, 'cy': 46}
(3, 4) 7
2 1
0 zero
1 one
[(1, 'a'), (2, 'b'), (3, 'c')]
['apple', 'date', 'Fig', 'pear'] ['Fig', 'pear', 'date', 'apple']
True True
```

`d["missing"]` panics, like Python's `KeyError`. `d.get(key)` returns an optional (see [section 8](#8-optional-values)) and `d.get_or(key, default)` a default.

An empty literal needs its type written down, because there is nothing to infer it from: `names: list[str] = []`.

Lists can be counted, repeated with `*` and reversed into a new list, and `list(...)` makes one from a string, a dict's keys or a `range`. A dict has `get_or_insert` (Python's `setdefault`), `update`, `copy` and `pop_or`.

```python
def main():
    scores = [3, 1, 3, 2, 3]
    print(scores.count(3), [0] * 4, 2 * ["ab"], reversed(scores), list("abc"), list(range(3)))
    groups: dict[str, list[int]] = {}
    for word in ["a", "bb", "cc", "d"]:
        groups.get_or_insert(str(len(word)), []).append(len(word))
    print(groups)
    base = {"a": 1, "b": 2}
    merged = base.copy()
    merged.update({"b": 20, "c": 3})
    print(base, merged, merged.pop_or("b", 0), merged.pop_or("zz", 0))
```

```text
3 [0, 0, 0, 0] ['ab', 'ab'] [3, 2, 3, 1, 3] ['a', 'b', 'c'] [0, 1, 2]
{'1': [1, 1], '2': [2, 2]}
{'a': 1, 'b': 2} {'a': 1, 'b': 20, 'c': 3} 20 0
```

Two immutable collections are for sharing data between tasks: `tuple[T, ...]`, a tuple of any length, and `frozendict[K, V]`. There is no immutable set; share a `set` through a `Mutex`.

A `set[T]` holds each element once, in the order it was added (Python's sets have an order of their own). `{a, b}` and `{x for x in items}` make one; an empty set is `set()`, since `{}` is a dict, and `set(items)` makes one from a list, a string, a dict's keys or a `range`. Elements need to be hashable, like dict keys.

```python
def main():
    seen: set[str] = set()
    for word in ["to", "be", "or", "not", "to", "be"]:
        seen.add(word)
    print(sorted(seen), len(seen), "or" in seen)

    evens = {n for n in range(10) if n % 2 == 0}
    small = {1, 2, 3}
    print(sorted(evens & small), sorted(evens | small), sorted(small - evens), sorted(evens ^ small))
    print(small.is_subset({1, 2, 3, 4}), small.is_disjoint({7}), small == {3, 2, 1})
    print(small.discard(2), small.discard(2), sorted(small), small.pop())
    print({5}, set("aab") == {"a", "b"}, sorted(set([3, 1, 3])))
```

```text
['be', 'not', 'or', 'to'] 4 True
[2] [0, 1, 2, 3, 4, 6, 8] [1, 3] [0, 1, 3, 4, 6, 8]
True True True
True False [1, 3] 1
{5} True [1, 3]
```

A set supports `in`, `len`, `for`, `sorted`, `sum`, `min`, `max` and `==`, and its methods are `add`, `discard` (which says whether the element was there), `remove` (panics if it is not), `pop`, `clear`, `copy`, `update`, `union`, `intersection`, `difference`, `symmetric_difference`, `is_subset`, `is_superset` and `is_disjoint`. The operators `|`, `&`, `-` and `^` give the union, intersection, difference and symmetric difference. The methods take sets, not any iterable as Python's do: write `a.union(set(items))`. Sets cannot be indexed, compared with `<`, used as dict keys or elements of other sets, and a set field in a struct with `@derive(Json)` is a JSON array.

## 7. Structs and methods

There are no classes. Data is declared with `struct`, and behaviour is added with `impl` blocks. Fields can have defaults. A method takes `self`; a function in an `impl` block without `self` is called on the type, like a static method.

```python
struct Point:
    x: int
    y: int

impl Point:
    def origin() -> Point:
        return Point(0, 0)

    def length_squared(self) -> int:
        return self.x * self.x + self.y * self.y

    def moved(self, dx: int, dy: int) -> Point:
        return Point(self.x + dx, self.y + dy)

struct User:
    name: str
    tags: list[str] = []
    visits: int = 0

impl User:
    def visit(self):
        self.visits += 1

@derive(Eq, Hash, Display)
struct Card:
    rank: int
    suit: str

def main():
    p = Point(3, 4)
    print(p.length_squared(), p.moved(1, 1).x, Point.origin().y)

    ada = User(name="Ada")
    ada.visit()
    ada.visit()
    ada.tags.append("admin")
    print(ada.name, ada.visits, ada.tags)

    same = ada
    same.name = "Ada Lovelace"
    print(ada.name, same is ada)

    card = Card(12, "hearts")
    print(card, card == Card(12, "hearts"))
    hands = {card: "mine"}
    print(hands[Card(12, "hearts")])
```

```text
25 4 0
Ada 2 ['admin']
Ada Lovelace True
Card(rank=12, suit='hearts') True
mine
```

Structs are passed by reference, as objects are in Python: `same = ada` makes a second name for the same user, and a function that changes a struct it was given changes the caller's struct.

`@derive(...)` writes the standard trait implementations for you:

| Derive | Gives |
|---|---|
| `Display` | Printing and `str(x)`, as `Card(rank=12, suit='hearts')` |
| `Eq` | `==` and `!=`, comparing field by field. Without it, `==` on structs is a compile error; use `is` for identity |
| `Hash` | Use as a dict key (needs `Eq`) |
| `Ord` | `<`, `>`, `sorted`, `min` and `max`, comparing fields in order |
| `Copy` | A `.copy()` method making a shallow copy |
| `Json` | Encoding to and decoding from JSON ([section 17](#17-json)) |

`@frozen` makes every field read-only after construction. Frozen values can be shared between tasks ([section 16](#16-tasks-channels-and-shared-state)).

## 8. Optional values

`None` is not a value of every type. A variable that might hold nothing has an optional type, written `T?`, and the compiler makes you deal with the `None` before using the value.

```python
struct Person:
    name: str
    manager: Person? = None

def find(people: list[Person], name: str) -> Person?:
    for p in people:
        if p.name == name:
            return p
    return None

def manager_name(people: list[Person], name: str) -> str?:
    person = find(people, name)?
    boss = person.manager?
    return boss.name

def main():
    grace = Person("Grace")
    people = [grace, Person("Ada", grace)]

    found = find(people, "Ada")
    if found is not None:
        print("found", found.name)

    print(find(people, "Linus") is None)
    print(manager_name(people, "Ada"), manager_name(people, "Grace"), manager_name(people, "Linus"))

    nothing: int? = None
    five: int? = 5
    print(five.unwrap(), nothing.unwrap_or(0))

    match find(people, "Grace"):
        case Some(person):
            print("matched", person.name)
        case None:
            print("nobody")
```

```text
found Ada
True
Grace None None
5 0
matched Grace
```

There are four ways to get at the value inside an optional:

- **Narrowing:** after `if x is not None:`, the compiler treats `x` as a plain value inside the block.
- **`?`:** returns `None` from the current function if the value is `None`.
- **`unwrap_or(default)`:** gives a fallback. `unwrap()` also exists, and panics on `None`.
- **`match`:** with `case Some(value):` and `case None:`.

## 9. Enums and `match`

An enum is a type with a fixed set of variants, and each variant can carry fields. `match` takes a value apart. The compiler checks that the cases cover every possibility, so adding a variant shows you every `match` that needs updating.

```python
@derive(Display)
enum Shape:
    Circle(radius: float)
    Rect(width: float, height: float)
    Dot

def area(shape: Shape) -> float:
    match shape:
        case Circle(r):
            return 3.14159 * r * r
        case Rect(w, h):
            return w * h
        case Dot:
            return 0.0

def size(n: int) -> str:
    match n:
        case 0:
            return "zero"
        case k if k < 0:
            return "negative"
        case 1 | 2 | 3:
            return "small"
        case _:
            return "big"

def where(point: tuple[int, int]) -> str:
    match point:
        case (0, 0):
            return "origin"
        case (0, _) | (_, 0):
            return "on an axis"
        case _:
            return "elsewhere"

def main():
    shapes = [Circle(1.0), Rect(2.0, 3.0), Dot]
    for shape in shapes:
        print(shape, f"{area(shape):.2f}")
    print(size(0), size(-4), size(2), size(10))
    print(where((0, 0)), where((0, 5)), where((2, 3)))
```

```text
Circle(radius=1.0) 3.14
Rect(width=2.0, height=3.0) 6.00
Dot 0.00
zero negative small big
origin on an axis elsewhere
```

A `match` can also give a value. As the whole right-hand side of an assignment or a `return`, each `case` has its value on its own line, and the cases must cover everything.

```python
def label(code: int) -> str:
    return match code:
        case 200: "ok"
        case 404: "not found"
        case _: "other"

def main():
    print(label(200), label(404), label(500))
    n = 7
    parity = match n % 2:
        case 0: "even"
        case _: "odd"
    print(n, parity)
```

```text
ok not found other
7 odd
```

Patterns can be literals, variants with their fields, tuples, `_` for anything, a name that binds the value, alternatives joined by `|`, and a guard written `if`. Variants are usually written bare (`Circle(1.0)`) and can be qualified (`Shape.Circle(1.0)`) where two enums share a variant name.

## 10. Errors and panics

There are no exceptions and no `try`. A function that can fail returns `Result[T]`, which is either `Ok(value)` or `Err(error)`. Inside a function that also returns a `Result`, `?` after a call passes an error straight back to the caller. Ignoring a `Result` is a compile error; write `_ = f()` to discard one on purpose.

```python
struct InsufficientFunds:
    needed: int

impl Error for InsufficientFunds:
    def message(self) -> str:
        return f"insufficient funds: {self.needed} more needed"

def withdraw(balance: int, amount: int) -> Result[int]:
    if amount > balance:
        return Err(InsufficientFunds(amount - balance))
    return balance - amount

def withdraw_text(balance: int, text: str) -> Result[int]:
    amount = int.parse(text)?
    return withdraw(balance, amount)?

def main():
    for request in ["30", "500", "lots"]:
        match withdraw_text(100, request):
            case Ok(left):
                print(f"withdrew {request}, {left} left")
            case Err(error):
                print(f"{request}: {error.message()}")

    print(withdraw(100, 10).unwrap(), withdraw(100, 200).is_err())
    print(int.parse("x").unwrap_or(-1))
```

```text
withdrew 30, 70 left
500: insufficient funds: 400 more needed
lots: "lots" is not a valid int
90 True
-1
```

A value of any type that implements the `Error` trait can be an error: it needs a `message` method. Returning a plain value from a function that returns `Result[int]` wraps it in `Ok` for you. `Result[T, E]` names a specific error type, where callers need to know it.

Mistakes in the program itself, such as an index out of range, a missing dict key, dividing by zero or `unwrap()` on `None`, are panics rather than errors. A panic prints a message with the source location and a backtrace, and ends the task it happens in:

```text
panic: index 10 is out of range for a list of length 3
  --> shop.wb:3:12
backtrace:
    pick at shop.wb:3:18
    main at shop.wb:6:16
```

In a web server, a panic in one handler becomes a 500 response and the server carries on.

## 11. Traits

A trait is a set of methods that types can implement. Traits can supply default methods, and can require other traits. A trait used as a type, like `list[Shape]` below, holds any value that implements it, and calls go to the right implementation at run time.

```python
trait Shape(Display):
    def area(self) -> float
    def name(self) -> str

    def describe(self) -> str:
        return f"{self.name()} with area {self.area():.1f}"

@derive(Display)
struct Circle:
    radius: float

@derive(Display)
struct Square:
    side: float

impl Shape for Circle:
    def area(self) -> float:
        return 3.14159 * self.radius ** 2

    def name(self) -> str:
        return "circle"

impl Shape for Square:
    def area(self) -> float:
        return self.side * self.side

    def name(self) -> str:
        return "square"

    def describe(self) -> str:
        return f"a {self.side}-wide square"

@derive(Display)
struct Money:
    cents: int

impl AddNumber for Money:
    def add(self, other: Money) -> Money:
        return Money(self.cents + other.cents)

def total_area(shapes: list[Shape]) -> float:
    total = 0.0
    for shape in shapes:
        total += shape.area()
    return total

def main():
    shapes: list[Shape] = [Circle(1.0), Square(2.0)]
    for shape in shapes:
        print(shape.describe())
    print(f"total: {total_area(shapes):.2f}")
    print(shapes[0])
    print(Money(1000) + Money(250))
```

```text
circle with area 3.1
a 2.0-wide square
total: 7.14
Circle(radius=1.0)
Money(cents=1250)
```

`trait Shape(Display)` means every shape must also be printable, so `print(shapes[0])` works. Implementing `Display` by hand means writing `def to_str(self) -> str`.

Operators are traits too: `AddNumber` gives `+`, `MulNumber` gives `*`, and so on. `Eq`, `Ord` and `Hash` can be written by hand instead of derived.

## 12. Generics

Functions, structs, enums and `impl` blocks can take type parameters in square brackets. A bound such as `T: Ord` says what the type must support. Type arguments are inferred from the values where possible.

```python
struct Stack[T]:
    items: list[T]

impl[T] Stack[T]:
    def new() -> Stack[T]:
        return Stack(items=[])

    def push(self, item: T):
        self.items.append(item)

    def pop(self) -> T?:
        if len(self.items) == 0:
            return None
        return self.items.pop()

impl[T: Display] Display for Stack[T]:
    def to_str(self) -> str:
        return f"Stack{self.items}"

def largest[T: Ord](items: list[T]) -> T?:
    if len(items) == 0:
        return None
    best = items[0]
    for item in items:
        if item > best:
            best = item
    return best

def map_list[T, U](items: list[T], f: def(T) -> U) -> list[U]:
    return [f(item) for item in items]

enum Tree[T]:
    Leaf
    Node(left: Tree[T], value: T, right: Tree[T])

def insert[T: Ord](tree: Tree[T], value: T) -> Tree[T]:
    match tree:
        case Leaf:
            return Node(Leaf, value, Leaf)
        case Node(left, v, right):
            if value < v:
                return Node(insert(left, value), v, right)
            return Node(left, v, insert(right, value))

def size[T](tree: Tree[T]) -> int:
    match tree:
        case Leaf:
            return 0
        case Node(left, _, right):
            return size(left) + 1 + size(right)

def main():
    s: Stack[int] = Stack.new()
    s.push(1)
    s.push(2)
    print(s, s.pop(), s.pop(), s.pop())

    print(largest([3, 9, 2]), largest(["b", "c", "a"]))
    print(map_list([1, 2, 3], lambda n: f"#{n}"))

    tree: Tree[int] = Leaf
    for n in [5, 3, 8, 1]:
        tree = insert(tree, n)
    print(size(tree))
```

```text
Stack[1, 2] 2 1 None
9 c
['#1', '#2', '#3']
4
```

The bounds in the standard library are `Eq`, `Ord`, `Hash`, `Display` and `Number` (anything with arithmetic). Generic code is compiled once per type it is used with, so it runs as fast as code written for that type.

## 13. Iterators

`for` works on lists, dicts, strings, ranges and channels. It also works on your own types: give a type a `next(self) -> T?` method that returns `None` when it is done, and `for` will call it. A type with an `iter(self)` method that returns such an iterator can be looped over directly.

```python
struct Countdown:
    current: int

impl Countdown:
    def next(self) -> int?:
        if self.current <= 0:
            return None
        self.current -= 1
        return self.current + 1

struct Fibonacci:
    a: int
    b: int
    limit: int

impl Fibonacci:
    def next(self) -> int?:
        if self.a > self.limit:
            return None
        value = self.a
        self.a, self.b = self.b, self.a + self.b
        return value

def main():
    for n in Countdown(3):
        print("countdown", n)
    print([f for f in Fibonacci(0, 1, 50)])
```

```text
countdown 3
countdown 2
countdown 1
[0, 1, 1, 2, 3, 5, 8, 13, 21, 34]
```

## 14. Modules and packages

Each file is a module. `import a.b` loads `a/b.wb` relative to the main file, and `from a.b import name` imports single names. Two keywords control what other files can see. A top-level declaration is private to its file unless it is marked `pub`. Fields and methods are visible wherever their type is, unless marked `priv`. A single-file program never needs either.

A project laid out as:

```text
main.wb
calc/
    syntax.wb
    parser.wb
```

`calc/syntax.wb`:

```python
pub enum Expr:
    Num(value: float)
    Add(left: Expr, right: Expr)

pub struct ParseError:
    priv reason: str

impl Error for ParseError:
    def message(self) -> str:
        return self.reason

pub def show(e: Expr) -> str:
    match e:
        case Num(value):
            return str(value)
        case Add(left, right):
            return f"({show(left)} + {show(right)})"
```

`calc/parser.wb` starts with `from calc.syntax import Expr, ParseError` and defines `pub def parse(text: str) -> Result[Expr]`.

`main.wb`:

<!-- not compiled: imports files that are not on this page -->
```python
import calc.syntax as syntax
from calc.parser import parse

def main():
    print(syntax.show(parse("1 + 2").unwrap()))
```

Standard library modules are imported the same way: `import json`, `import http`. For dependencies from git or another folder, a `weby.toml` next to the main file lists them; see the [README](../README.md#building-and-running). `weby fetch` downloads them and records their exact commits in `weby.lock`.

## 15. Files, the environment and `with`

The `io` module reads and writes files, and `os` gives the command-line arguments, environment variables and exit status. File operations return `Result`, since files go missing.

```python
import io
import os

def main():
    path = "/tmp/example-guide-notes.txt"
    io.write_file(path, "first\nsecond\n").unwrap()
    io.append_file(path, "third\n").unwrap()
    print(io.read_lines(path).unwrap())

    with io.open(path).unwrap() as f:
        print("first line:", f.read_line().unwrap())

    match io.read_file("/no/such/file"):
        case Ok(_):
            print("unexpected")
        case Err(error):
            print("error:", error.message())

    io.remove(path).unwrap()
    print("still there:", io.exists(path))
    print("HOME set:", os.getenv("HOME") is not None)
    print("arguments:", len(os.args()) - 1)
```

```text
['first', 'second', 'third']
first line: first
error: /no/such/file: No such file or directory
still there: False
HOME set: True
arguments: 0
```

A `with` block runs a value's `exit` method when the block ends, including by `return` or `break`. So the file above is closed when the block ends. Any type gets this by having an `exit` method, and optionally an `enter` method whose result is bound by `as`.

`io.eprint` writes to standard error, `io.read_line()` reads a line from standard input, and `os.exit(code)` ends the program.

C functions can be called directly by declaring them with `@extern`:

```python
@extern("labs")
def c_labs(x: int) -> int

def main():
    print(c_labs(-5))
```

```text
5
```

## 16. Tasks, channels and shared state

`spawn f(args)` runs a function call as a task, concurrently with the caller, and returns a `Task` whose `join()` waits for the result. Tasks are cheap: they are fibers spread over one thread per CPU core, and a task waiting for the network or a channel costs no thread.

```python
def slow_square(n: int) -> int:
    return n * n

def main():
    tasks = [spawn slow_square(n) for n in range(5)]
    print([t.join() for t in tasks])
```

```text
[0, 1, 4, 9, 16]
```

The compiler stops tasks from sharing data that can change: passing a mutable struct, list or dict to `spawn` is a compile error. What can be shared is:

- numbers, strings and booleans;
- `@frozen` structs, and tuples, optionals and enums, as long as everything inside them can be shared too;
- the immutable collections `tuple[T, ...]` and `frozendict[K, V]`;
- `Channel`, `Mutex`, `Atomic` and `Task` values.

So a data race cannot be written by accident.

A `Channel[T]` passes values between tasks. Here four workers take jobs from one channel and send results to another:

```python
@frozen
struct Job:
    id: int
    value: int

@frozen
struct Outcome:
    job: int
    square: int

def worker(jobs: Channel[Job], results: Channel[Outcome]):
    for job in jobs:
        results.send(Outcome(job.id, job.value * job.value))

def feed(jobs: Channel[Job], count: int):
    for i in range(count):
        jobs.send(Job(i, i))
    jobs.close()

def main():
    jobs = Channel[Job](16)
    results = Channel[Outcome](16)
    pool = [spawn worker(jobs, results) for _ in range(4)]
    feeder = spawn feed(jobs, 100)

    outcomes: list[Outcome] = []
    for _ in range(100):
        outcomes.append(results.recv().unwrap())
    feeder.join()
    for task in pool:
        task.join()

    outcomes.sort(key=lambda o: o.job)
    print(len(outcomes), sum([o.square for o in outcomes]))
    print([(o.job, o.square) for o in outcomes[:4]])
```

```text
100 328350
[(0, 0), (1, 1), (2, 4), (3, 9)]
```

The jobs are sent from their own task: if `main` sent all 100 itself before reading any results, the workers would fill the results channel and wait, and `main` would wait for them. `Channel[T](16)` holds up to 16 values before `send` waits; `Channel[T]()` makes every `send` wait for a receiver. `for job in jobs` ends when the channel is closed and empty. `recv()` returns `None` once that happens, and `try_recv()` returns `None` straight away if nothing is waiting.

For state that several tasks update, use a `Mutex` or an `Atomic`:

```python
def hit(counter: Mutex[int], tally: Mutex[dict[str, int]], hits: Atomic, name: str):
    for _ in range(1000):
        with counter.lock() as n:
            n += 1
        hits.increment()
    with tally.lock() as totals:
        totals[name] = totals.get_or(name, 0) + 1

def main():
    counter = Mutex(0)
    tally: Mutex[dict[str, int]] = Mutex({})
    hits = Atomic(0)
    tasks = [spawn hit(counter, tally, hits, "even" if i % 2 == 0 else "odd") for i in range(50)]
    for task in tasks:
        task.join()
    with counter.lock() as n:
        print("mutex:", n)
    print("atomic:", hits.get())
    with tally.lock() as totals:
        print(totals["even"], totals["odd"])
```

```text
mutex: 50000
atomic: 50000
25 25
```

The value inside a `Mutex` can only be reached inside a `with m.lock() as value:` block, and the compiler rejects code that lets it escape the block. A panic in a task ends only that task and releases any lock it held; `join()` on that task then panics in the joiner.

## 17. JSON

`@derive(Json)` generates an encoder and a decoder for a struct or enum. `json.decode[T](text)` checks the document against the type and says where it doesn't fit.

```python
import json

@derive(Json, Display)
enum Contact:
    Email(address: str)
    Phone(number: str)

@derive(Json, Display)
struct Customer:
    id: int
    name: str
    @json(name="emailOptIn")
    opt_in: bool = False
    nickname: str? = None
    contacts: list[Contact] = []

def main():
    ada = Customer(id=7, name="Ada", contacts=[Email("ada@example.org")])
    text = json.encode(ada)
    print(text)

    back = json.decode[Customer]('{"id": 8, "name": "Bo", "emailOptIn": true}').unwrap()
    print(back)

    for broken in ['{"id": "8", "name": "Bo"}', '{"id": 8}', '{"id": 8, "name": "Bo", "contacts": [{"type": "Fax"}]}']:
        match json.decode[Customer](broken):
            case Ok(_):
                print("decoded")
            case Err(error):
                print("error:", error.message())

    data = json.parse('{"order": {"id": 42, "items": [{"sku": "A-1"}, {"sku": "B-7"}]}}').unwrap()
    order = data["order"]
    print(order["id"].as_int(), [item["sku"].as_str().unwrap_or("?") for item in order["items"].as_list().unwrap_or([])])
    print(data["order"]["missing"]["deeper"].is_null())
```

```text
{"id":7,"name":"Ada","emailOptIn":false,"nickname":null,"contacts":[{"type":"Email","address":"ada@example.org"}]}
Customer(id=8, name='Bo', opt_in=True, nickname=None, contacts=[])
error: id: expected an integer, found a string
error: missing key "name"
error: contacts[0]: unknown variant; expected one of "Email", "Phone"
42 ['A-1', 'B-7']
True
```

Fields with defaults may be left out of the document. Optional fields accept `null`. Unknown keys are ignored unless the struct is marked `@json(strict)`. Enum variants are encoded with a `"type"` key.

For documents whose shape isn't known in advance, `json.parse` gives a `json.Value`. Indexing a `json.Value` never fails: a missing key gives `null`, and `as_int()`, `as_str()` and the other accessors return optionals. `json.object({...})` builds a value from literals.

## 18. A web service

The `http` module is an HTTP/1.1 server. The shortest way to write a service is to annotate functions or methods with their routes, then `mount` them. The handler's parameters say what it needs, and the compiler checks them against the route:

- path segments such as `{id}` become typed parameters;
- other simple parameters come from the query string;
- a parameter named `body` is the request body, decoded from JSON;
- `http.Header` and `http.Cookie` parameters come from the request's headers and cookies.

If a request doesn't fit, it gets a 400 response with a message saying why, before your code runs. The return value becomes the response: a struct with `@derive(Json)` is sent as JSON, and a `str` as text.

Here is a small notes service. Because `server.handle(...)` answers a request without a network connection, this program can test its own routes, which is also how you would write tests for a service:

```python
import http

@derive(Json)
@frozen
struct Note:
    id: int
    text: str

@derive(Json)
@frozen
struct NewNote:
    text: str

@frozen
struct Notes:
    store: Mutex[dict[int, Note]]
    next_id: Atomic

impl Notes:
    @http.get("/notes")
    def list(self, limit: int = 10) -> list[Note]:
        with self.store.lock() as store:
            return [note for note in store.values()][:limit]

    @http.get("/notes/{id}")
    def get(self, id: int) -> Result[Note]:
        with self.store.lock() as store:
            match store.get(id):
                case Some(note):
                    return note
                case None:
                    return Err(http.Error(404, "no such note"))

    @http.post("/notes", status=201)
    def create(self, body: NewNote) -> Result[Note]:
        if body.text.strip() == "":
            return Err(http.Error(422, "the note is empty"))
        note = Note(self.next_id.increment(), body.text)
        with self.store.lock() as store:
            store[note.id] = note
        return note

@http.get("/health")
def health() -> str:
    return "ok"

def main():
    server = http.Server()
    server.mount(Notes(Mutex({}), Atomic(0)), prefix="/api")
    server.mount(health)

    for method, path, body in [
        ("POST", "/api/notes", '{"text": "buy milk"}'),
        ("POST", "/api/notes", '{"text": "  "}'),
        ("POST", "/api/notes", '{"txt": "typo"}'),
        ("GET", "/api/notes/1", ""),
        ("GET", "/api/notes/9", ""),
        ("GET", "/api/notes/abc", ""),
        ("GET", "/api/notes", ""),
        ("GET", "/health", ""),
        ("DELETE", "/health", ""),
    ]:
        response = server.handle(http.test_request(method, path, body))
        print(method, path, response.status, response.body)
```

```text
POST /api/notes 201 {"id":1,"text":"buy milk"}
POST /api/notes 422 {"error":"the note is empty"}
POST /api/notes 400 {"error":"invalid JSON body: missing key \"text\""}
GET /api/notes/1 200 {"id":1,"text":"buy milk"}
GET /api/notes/9 404 {"error":"no such note"}
GET /api/notes/abc 400 {"error":"path parameter `id` must be an integer"}
GET /api/notes 200 [{"id":1,"text":"buy milk"}]
GET /health 200 ok
DELETE /health 405 {"error":"method not allowed"}
```

To serve it for real, replace the loop with:

```python
server.listen(":8080").unwrap()
```

Each connection is handled in its own task, so the handlers run concurrently. That is why `Notes` is `@frozen` and keeps its data behind a `Mutex`: the compiler checks that whatever a server shares is safe to share. `listen` serves until the process gets SIGINT or SIGTERM, then lets requests in progress finish.

Routes can also be registered by calling methods, with handlers that take the `http.Request` and return `Result[http.Response]`. This style gives full control over the request and the response, and adds middleware (code that runs around every request) and route groups:

```python
import http
import log
import time

def hello(req: http.Request) -> Result[http.Response]:
    name = req.query("name").unwrap_or("world")
    return http.text(f"hello {name}")

def item(req: http.Request) -> Result[http.Response]:
    id = req.param_int("id")?
    if id > 100:
        return Err(http.Error(404, "no such item"))
    return http.json({"id": id}).with_header("Cache-Control", "max-age=60")

def timing(req: http.Request, next: http.Handler) -> Result[http.Response]:
    start = time.now()
    response = next(req)?
    log.info(f"{req.method} {req.path} {response.status} {time.since(start):.4f}s")
    return response

def main():
    server = http.Server()
    server.use(timing)
    server.get("/hello", hello)
    api = server.group("/api/v1")
    api.get("/items/{id}", item)
    server.listen_tls(":8443", "cert.pem", "key.pem").unwrap()
```

`listen_tls` serves HTTPS from a PEM certificate and key; `listen` serves plain HTTP. Errors map to responses in one place:

- an `http.Error(status, message)` keeps its status;
- a request that doesn't fit its handler is a 400;
- any other error is a 500, logged with its message, and the client gets a generic one.

`server.on_error(...)` replaces this mapping.

## 19. WebSockets

`@http.ws(path)` declares a WebSocket endpoint. The handler runs in its own task for the life of the connection, and `for msg in ws` ends when the client closes. The runtime handles pings, fragmentation and the closing handshake, and the implementation passes the Autobahn conformance suite.

```python
import http

@frozen
struct Hub:
    clients: Mutex[dict[int, http.WebSocket]]

impl Hub:
    @http.ws("/chat/{room}")
    def chat(self, ws: http.WebSocket, room: str, name: str = "anon"):
        with self.clients.lock() as clients:
            clients[ws.id] = ws
        self.broadcast(f"[{room}] {name} joined")

        for msg in ws:
            match msg:
                case Text(body):
                    self.broadcast(f"[{room}] {name}: {body}")
                case Binary(_):
                    ws.close(1003, "text only")

        with self.clients.lock() as clients:
            clients.pop(ws.id)
        self.broadcast(f"[{room}] {name} left")

    def broadcast(self, text: str):
        with self.clients.lock() as clients:
            for client in clients.values():
                _ = client.send(text)

def main():
    server = http.Server()
    server.mount(Hub(Mutex({})))
    server.listen(":8080").unwrap()
```

A `WebSocket` can be sent to from any task, which is how `broadcast` reaches every client. `send` returns a `Result`, because the client may have gone; `_ = ...` ignores it on purpose. An idle connection costs about 11 KiB, so one server can hold tens of thousands.

## 20. The HTTP client

`http.get`, `http.post` and `http.fetch` make requests to `http://` and `https://` URLs. Certificates are checked against the Mozilla root certificates built into the program, so it doesn't depend on the system's OpenSSL.

```python
import http

@derive(Json)
struct Repo:
    name: str
    stargazers_count: int

def main():
    response = http.get("https://api.github.com/repos/rust-lang/rust").unwrap()
    if response.ok():
        repo = http.decode_json[Repo](response).unwrap()
        print(repo.name, repo.stargazers_count)

    headers = {"Authorization": "Bearer token", "Accept": "application/json"}
    reply = http.fetch("POST", "https://example.com/api", '{"x": 1}', headers).unwrap()
    print(reply.status, reply.header("Content-Type"), reply.text().unwrap())
```

## 21. PostgreSQL

The `postgres` module is a PostgreSQL client built into the runtime, so it doesn't need `libpq`. `postgres.connect` opens a connection pool that any number of tasks can share. Parameters are passed separately from the SQL, so values never need escaping. They are a list of strings, so a number is written `str(n)`. Rows can be decoded straight into `@derive(Json)` structs by column name.

```python
import os
import postgres

@derive(Json)
struct User:
    id: int
    name: str
    score: float?

def main():
    db = postgres.connect(os.getenv("DATABASE_URL").unwrap()).unwrap()
    db.execute("insert into users (name, score) values ($1, $2)", ["ann", "9.5"]).unwrap()

    users = postgres.query_as[User](db, "select id, name, score from users order by id").unwrap()
    for user in users:
        print(user.id, user.name, user.score)

    for row in db.query("select name from users where score > $1", ["5"]).unwrap():
        print(row.text("name").unwrap())

    with db.transaction().unwrap() as tx:
        tx.execute("update users set score = score + 1 where name = $1", ["ann"]).unwrap()
        tx.commit().unwrap()
```

A transaction that leaves its `with` block without `commit()` is rolled back. Database errors carry PostgreSQL's SQLSTATE code (`error.code`, such as `23505` for a unique violation) and its message. The client supports SCRAM authentication and TLS. URL options set TLS (`?sslmode=verify-full`) and the pool size (`pool_max=20`; the default is 10).

## 22. Regular expressions, time and logging

```python
import log
import re
import time

def main():
    date = re.compile(r"(?P<year>\d{4})-(?P<month>\d{2})").unwrap()
    m = date.find("due 2026-11-18").unwrap()
    print(m.text, m.named("year"), m.group(2).unwrap())
    print(date.replace_all("2026-11 and 2027-01", "$month/$year"))
    print([x.text for x in re.compile("[0-9]+").unwrap().find_all("3, 14 and 159")])

    start = time.monotonic()
    time.sleep(0.01)
    print("slept at least 10 ms:", time.monotonic() - start >= 0.01)

    log.set_level("warn")
    log.info("not shown")
    log.warn("shown on standard error")
```

```text
2026-11 2026 11
11/2026 and 01/2027
['3', '14', '159']
slept at least 10 ms: True
```

`re` has the usual Perl-style syntax, including named groups, but no backreferences or lookaround. That is what lets it guarantee matching in time linear in the text, so a pattern can't make a server spin. A compiled `re.Regex` can be shared between tasks. Patterns are ordinary strings, so a raw string, `r"\d+"`, saves doubling the backslashes.

`log` writes one line per message to standard error, with a UTC timestamp and the level: `debug`, `info`, `warn` or `error`. The default level is `info`. `log.set_level` changes it, as does the `WEBY_LOG` environment variable.

## 23. Writing tests

Mark a function `@test` and run `weby test`. A file of tests, `adder.wb`: Each test runs in its own task, so a panicking test fails without stopping the others. `assert condition, message` fails a test. A test that returns `Result[None]` can use `?`, and fails if it returns an error.

```python
def add(a: int, b: int) -> int:
    return a + b

@test
def adds_small_numbers():
    assert add(2, 3) == 5

@test
def adds_negative_numbers():
    assert add(-2, -3) == -5, f"got {add(-2, -3)}"

@test
def parses_what_it_prints() -> Result[None]:
    n = int.parse(str(add(40, 2)))?
    assert n == 42
```

```bash
weby test
```

```text
running 3 tests in ./adder.wb
test adds_small_numbers ... ok
test adds_negative_numbers ... ok
test parses_what_it_prints ... ok

test result: ok. 3 passed; 0 failed
```

`weby test` with no arguments runs the tests in every `.wb` file under the current folder. `--filter text` runs only the tests whose names contain `text`.

## 24. Tools

| Command | Does |
|---|---|
| `weby run file.wb` | Compile and run |
| `weby build file.wb [-o out] [--release]` | Compile to an executable |
| `weby check file.wb` | Report errors without generating code (fast) |
| `weby test [paths] [--filter text]` | Run `@test` functions |
| `weby fmt [paths] [--check]` | Format files in the standard layout |
| `weby fetch [--update]` | Fetch git dependencies and update `weby.lock` |
| `weby lsp` | The language server: diagnostics, hover, go to definition, formatting |
| `weby build --emit=tokens\|ast\|hir\|llvm-ir\|obj file.wb` | Show an intermediate form |

Compiler messages show the source and usually suggest a fix:

```text
error[E0317]: a condition must be a `bool`, found `int`
 --> shop.wb:3:8
  |
3 |     if len(items):
  |        ^^^^^^^^^^ has type `int`
  |
  = help: values are not true or false by themselves; compare explicitly, for example `len(items) > 0`
```

Programs include debug line information, so `gdb` and `perf` show function names and source lines. Editor support (a Zed extension, VS Code, Vim, Neovim and Helix) is described in [editors/README.md](../editors/README.md).

## 25. Coming from Python

| In Python | In Weby |
|---|---|
| `class User:` | `struct User:` for data, `impl User:` for methods, `trait` for interfaces |
| Inheritance | Traits with default methods; a trait can require other traits |
| `try` / `except` / `raise` | Return `Result[T]`; `?` passes errors on; `match` handles them |
| `None` anywhere | Only in optional types, `T?`; the compiler checks for `None` before use |
| `if items:` | `if len(items) > 0:`; conditions must be `bool` |
| Unbounded `int` | 64-bit `int` that wraps around |
| `1 + 2.5` | `float(1) + 2.5`; an `int` and a `float` don't mix silently |
| `x.startswith(...)` | `x.starts_with(...)` |
| `d.get(k, default)` | `d.get_or(k, default)`; `d.get(k)` returns `V?` |
| `d.setdefault(k, v)` | `d.get_or_insert(k, v)` |
| `s.splitlines()`, `s.isdigit()`, `s.removeprefix(p)` | `s.split_lines()`, `s.is_digit()`, `s.remove_prefix(p)` |
| `d.pop(k, default)` | `d.pop_or(k, default)`; `d.pop(k)` returns `V?` |
| `list(range(n))`, `reversed(xs)`, `[0] * n`, `del xs[i]` | The same; `reversed` gives a list |
| `__eq__`, `__hash__`, `__repr__` | `@derive(Eq, Hash, Display)`, or implement the traits |
| `@dataclass(frozen=True)` | `@frozen` |
| `threading` / `asyncio` | `spawn`, `Channel`, `Mutex`, `Atomic`; no `async` or `await`, and no global interpreter lock |
| Code at the top level of a file | Declarations only; the program starts in `main` |
| `set` | `set[T]`; `{1, 2}`, `set()` for an empty one, and `discard` says whether the element was there |

## Where to go next

- [tests/run](../tests/run) has about forty more programs, each with its expected output in comments at the top.
- [tests/http](../tests/http) has complete services: a todo REST API written in both styles, and a chat server.
- [docs/01](01-language-design.md) and [docs/02](02-type-system-and-traits.md) are the language design and type system in full; [docs/05](05-standard-library.md) is the standard library.
