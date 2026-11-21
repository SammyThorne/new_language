# 02 — Type system and traits

## Summary

- Static and nominal. Two structs with the same fields are different types.
- Function signatures and struct fields are annotated. Everything inside a function body is inferred.
- No implicit conversions between unrelated types. Three implicit *coercions* exist, all of which add information and never lose it:
  1. `T` to `T?` (wrap in `Some`),
  2. `T` to `Result[T, E]` (wrap in `Ok`),
  3. a struct to a trait it implements (build a trait object).
- Generics are monomorphised. A trait used directly as a type is dynamically dispatched.
- No subtyping between structs. No null.

## Typing rules

Viper is statically typed and uses Python's type-hint syntax. Every expression has a type known at compile time. A missing type where one is required, or a mismatch anywhere, is a compile error; there are no runtime type errors, no `Any`, and no `isinstance`.

### Where a type hint is required

| Place | Hint | If it is missing |
|---|---|---|
| Function and method parameters (other than `self`) | Always required | Compile error |
| Function return | `-> type` when the function returns a value. No `->` means `-> None` | Compile error on `return value` |
| Struct fields | Always required | Compile error |
| Enum variant fields | Always required | Compile error |
| Local variables | Optional. The type is taken from the first assignment and then fixed | Compile error only when the value does not determine the type: `[]`, `{}`, `None` |
| `const` | Optional; taken from the constant value | |
| `lambda` parameters | Never written; taken from where the lambda is used | Compile error if the use does not determine them |

A local variable without a hint is still statically typed. `count = 0` declares an `int` exactly as `count: int = 0` does.

### What is a compile error

```python
def test(a: int) -> str:
    return a                    # error: expected `str`, found `int`

def area(w, h):                 # error: parameter `w` has no type hint
    return w * h                # error: function has no `->` but returns a value

struct User:
    id: int
    name                        # error: field `name` has no type hint

count = 0
count = "zero"                  # error: `count` is `int`, cannot assign `str`

total: int = 2.5                # error: expected `int`, found `float`
names: list[str] = [1, 2]       # error: expected `str`, found `int`
mixed = [1, "a"]                # error: list elements must have one type
test("5")                       # error: argument `a` expects `int`, found `str`
label: string = "x"             # error: unknown type `string`; did you mean `str`?
```

### Conversions are explicit

- Between numbers: `float(n)`, `int(x)` (truncates). Arithmetic never converts for you: with `n: int` and `x: float`, `n + x` is a compile error and `float(n) + x` is the fix. Literals are the one convenience: `x + 1` works because `1` is read as a `float` there.
- To text: `str(value)` for any type that implements `Display`.
- From text, which can fail: `int.parse(s)` and `float.parse(s)` return a `Result`.

### User-defined types

Structs are the types users define; enums and traits are the other two. Each is nominal: `struct Point` and `struct Size` are different types even with identical fields, and one is never accepted where the other is expected.

## Kinds of types

| Kind | Examples | Representation | Assignment |
|---|---|---|---|
| Scalar | `int`, `float`, `bool`, sized numerics | Machine value | Copies |
| String | `str`, `bytes` | Pointer and length to immutable GC memory | Copies the pair; shares the bytes |
| Tuple | `tuple[int, str]` | Inline aggregate | Copies |
| Struct | `User` | Pointer to a GC object | Shares the object |
| Enum | `Message`, `Option[T]`, `Result[T, E]` | Tag plus payload, inline; see below | Copies the tag and payload words |
| Collection | `list[T]`, `dict[K, V]`, `set[T]` | Pointer to a GC object | Shares the object |
| Trait object | `Shape` used as a type | Data pointer and vtable pointer | Copies the pair; shares the object |
| Function | `def(int) -> int` | Code pointer and environment pointer | Copies the pair |

### Enum layout

- Fieldless enums are a single small integer.
- General enums are a tag followed by storage sized for the largest variant.
- `Option` of any pointer-like type (struct, collection, `str`, trait object, function) uses the null pointer for `None`, so `User?` is one word and costs nothing.
- `Result[T, E]` with the default `E` (the `Error` trait object) is a tag plus the larger of `T` and two words.
- Large enums may be boxed by the compiler when a variant exceeds a size threshold. This is an implementation choice invisible to programs.

## Inference

Inference is local and bidirectional. It never crosses a function boundary.

- **Checking mode**: when the expected type is known (an annotated variable, an argument position, a return statement), the expression is checked against it. This is what gives `[]`, `None`, `lambda` parameters and numeric literals their types.
- **Synthesis mode**: otherwise the expression produces its own type.
- Inside one function body, unresolved pieces (the element type of `[]` assigned to an unannotated variable, a lambda's parameter types) are unification variables. If any remain unresolved at the end of the function, it is an error asking for an annotation.
- Integer literals default to `int`, and become `float` or a sized type when checked against one. Float literals default to `float`.
- Generic function calls infer type arguments from argument types and the expected result type. Explicit arguments (`json.decode[User](text)`) are needed only when nothing else determines them.

There is no global inference, no return type inference for named functions, and no overloading. These three omissions keep error messages local and the checker simple.

## Generics

(M4)

```python
def first[T](items: list[T]) -> T?:
    if len(items) == 0:
        return None
    return items[0]

def max_of[T: Ord](a: T, b: T) -> T:
    return a if a > b else b

struct Page[T]:
    items: list[T]
    next_cursor: str?

impl[T] Page[T]:
    def is_last(self) -> bool:
        return self.next_cursor is None

    def map[U](self, f: def(T) -> U) -> Page[U]:      # a method's own type parameter
        return Page(items=[f(x) for x in self.items], next_cursor=self.next_cursor)

def to_json_list[T: ToJson + Display](items: list[T]) -> str:
    ...
```

- Type parameters are declared in `[...]` after the name. Bounds follow `:` and combine with `+`.
- Inside a generic body, a value of type `T` supports only what its bounds provide. Bodies are type-checked once, against the bounds, before any instantiation, so errors are reported at the definition and not at each use.
- Each distinct set of type arguments produces its own compiled copy.
- Type arguments are inferred from a call's arguments and from the expected type, or written explicitly: `Page[int](items=[], next_cursor=None)`, `Stack[str].new()`, `first[int]([])`.
- Built-in types implement the prelude traits that describe what they support: `int` and `float` are `Number` (`+`, `-`, `*`) and `Ord`; `str`, `bool` and tuples of orderable types are `Ord`; and so on. `Number` is declared as `trait Number(AddNumber, SubNumber, MulNumber, Ord)`.
- The built-in collections are generic by compiler special-casing, and `Option` and `Result` are built in.
- A method can declare type parameters of its own as well as use its `impl` block's (D94): `page.map(lambda x: str(x))` infers `U` from the lambda. They are inferred only, from the arguments and the expected type; there is no `page.map[str](...)`. A method with no `self` can have them too (`Stack.of(items)`). A method of a trait cannot, since a trait can be used as a type.
- Traits cannot take type parameters, so iteration is a method protocol (D34).

Not supported: higher-kinded types, const generics, variadic generics, specialisation, generic methods on trait objects.

## Traits

### Declaring and implementing (M3)

```python
trait Store:
    def get(self, key: str) -> bytes?
    def put(self, key: str, value: bytes) -> Result[None]

trait Named:
    def name(self) -> str

trait Entity(Named, Eq):                # requires Named and Eq
    def id(self) -> int

impl Store for MemoryStore:
    def get(self, key: str) -> bytes?:
        return self.data.get(key)

    def put(self, key: str, value: bytes) -> Result[None]:
        self.data[key] = value
```

- A trait lists method signatures. A method with a body is a default and may be overridden.
- `trait A(B, C)` means every implementor of `A` must also implement `B` and `C`, and `A`'s default methods may call theirs.
- An `impl Trait for Type` block must define every required method with exactly matching signatures.
- Traits may declare methods without `self` (for example `def parse(text: str) -> Result[Self]`). These are callable through generics (`T.parse(s)`) and not through trait objects.
- `Self` inside a trait refers to the implementing type.

### Coherence

An `impl Trait for Type` is allowed only in the module that defines the trait or the module that defines the type. At most one implementation exists for any pair. This is a simple version of Rust's orphan rule and it guarantees that importing a module can never change which method a call resolves to.

### Method resolution

For `value.method(args)`:

1. Inherent methods of the value's type (`impl Type:` blocks).
2. Methods of traits the type implements, where the trait is imported or defined in the current module.
3. If the value's type is a type parameter, methods of its bounds.
4. If the value's type is a trait object, methods of that trait and its required traits.

Two candidates at the same step are an error; the call is then written `Trait.method(value, args)`.

### Static dispatch

A bounded type parameter is resolved at compile time. After monomorphisation, `max_of[int]` contains a direct integer comparison. This is the form to use in hot paths and inside collections.

### Dynamic dispatch

Using a trait name where a type is expected creates a trait object:

```python
def total_area(shapes: list[Shape]) -> float: ...

stores: dict[str, Store] = {"mem": MemoryStore(), "disk": DiskStore("/var/data")}
```

- A trait object is two words: a pointer to the struct and a pointer to a vtable. The vtable is a constant emitted once for each (type, trait) pair that is actually converted somewhere in the program.
- A method call loads the function pointer from the vtable and calls it with the data pointer.
- A trait can be used as a type only if it is **object safe**: no generic methods, and `Self` appears only as the receiver. The compiler reports which method breaks the rule.
- A trait object of `Entity` can be passed where `Named` is expected. Each vtable embeds or points to the vtables of its required traits.
- Recovering the concrete type is done with `match` on a type pattern (D94): `case Circle(c):` on a `Shape` binds `c` as the `Circle` when the object is one, and `case Circle():` only tests. The type is a struct or enum that implements the trait, and not generic. A `match` on a trait object is never exhaustive without `case _:` (or a name that binds the whole object), since any type can implement the trait. The test compares slot 0 of the vtable, below, with the type's descriptor. It should be rare: calling the trait's methods is the usual way.

Vtable layout:

| Slot | Content |
|---|---|
| 0 | Pointer to the concrete type's descriptor (name, size, type id) |
| 1..k | Pointers to the vtables of required traits |
| k+1.. | Method pointers in declaration order |

LLVM can turn a virtual call into a direct call when it sees the concrete type at the call site after inlining. The compiler also helps explicitly ([07](07-testing-and-performance.md#optimisation-backlog)).

### Generic implementations (M4)

```python
impl[T: Display] Display for list[T]:
    def to_str(self) -> str:
        return "[" + ", ".join([x.to_str() for x in self]) + "]"
```

Allowed when the trait or the type constructor is local to the module. Overlapping implementations are rejected.

### What the trait system deliberately lacks

To keep the checker and the language small, the prototype does not have:

- Associated types. `Iterator[T]` is a generic trait instead.
- Associated constants.
- Blanket implementations over all types (`impl[T: A] B for T`).
- Specialisation and negative bounds.
- Operator overloading beyond the fixed arithmetic and comparison traits below. New operators cannot be defined, and indexing, calling and attribute access cannot be overloaded.
- Implicit trait-based conversions (`From`/`Into`). The only conversion performed for the user is concrete error to `Error`.

Each of these has a real use and can be added later without breaking existing programs.

## Built-in traits

The built-in traits are defined in the prelude, `crates/lang_sema/src/prelude.lang`, a small file in the language itself that is compiled with every program. They are ordinary traits; the compiler gives them meaning, as the table says. The prelude also defines `ParseError`, the error type of `int.parse` and `float.parse`.

| Trait | Enables | Derivable | Milestone |
|---|---|---|---|
| `Display` | `str(x)`, f-string interpolation, `print(x)`; method `to_str(self) -> str` | yes, printed like a Python dataclass: `Point(x=1, y=2)` | M3 |
| `Eq` | `==`, `!=`; method `eq(self, other: Self) -> bool` | yes | M3 |
| `Ord` | `<`, `<=`, `>`, `>=`, `sorted`, `min`, `max` | yes (lexicographic) | M4 |
| `Hash` | Use as `dict` key or `set` element (requires `Eq`); method `hash(self) -> int` | yes | M3 |
| `Copy` | `x.copy()` shallow copy; method `copy(self) -> Self` | yes | M3 |
| `AddNumber`, `SubNumber`, `MulNumber` | `+`, `-`, `*` between two values of the same type | no | M3 |
| `Number` | Shorthand bound for `AddNumber + SubNumber + MulNumber + Ord` | no | M4 |
| `Shareable` | May be passed to another task. Decided by the compiler, never implemented by hand | automatic | M6 |
| `Iterable[T]` | `for x in value` | no | M4 |
| `Iterator[T]` | One method, `next(self) -> T?` | no | M4 |
| `Error` | Use as the error in `Result[T]`; method `message(self) -> str` | no | M3 |
| `Scoped[T]` | `with value as name:`; `enter(self) -> T` and `exit(self)` | no | M5 |
| `ToJson`, `FromJson` | `json.encode`, `json.decode`; both come from `@derive(Json)` | yes | M7 |

Built-in types implement the obvious ones: `int`, `float`, `bool`, `str` are `Display + Eq + Ord + Hash` (`float` is not `Hash`); `list[T]` is `Display`/`Eq` when `T` is; and so on.

### Derivation

`@derive(...)` asks the compiler to write the implementation from the struct's or enum's fields. It fails with a pointed message if a field's type does not implement the trait. Derivation is ordinary code generation into HIR, so derived methods are optimised and inlined like hand-written ones. This matters most for `Json`, where the generated encoder and decoder are what make typed JSON fast ([05](05-standard-library.md#json)).

### Arithmetic traits

Addition between numbers is defined by a built-in trait, and the other arithmetic operators follow the same pattern.

```python
trait AddNumber:
    def add(self, other: Self) -> Self

def total[T: AddNumber](items: list[T], start: T) -> T:
    result = start
    for item in items:
        result = result + item          # means AddNumber.add(result, item)
    return result
```

| Operator | Trait | Method |
|---|---|---|
| `a + b` | `AddNumber` | `add(self, other: Self) -> Self` |
| `a - b` | `SubNumber` | `sub(self, other: Self) -> Self` |
| `a * b` | `MulNumber` | `mul(self, other: Self) -> Self` |

- `int`, `float` and the sized numeric types implement all three.
- Both operands have the same type, because the parameter is `Self`. That is what makes `int + float` on variables a compile error, and it keeps the rule in one place.
- For the built-in number types the compiler emits the machine instruction directly. The trait exists for type checking and for generic code; it costs nothing at run time.
- Integer `add`, `sub` and `mul` wrap around on overflow.
- A user struct may implement these traits (a `Money` or `Decimal` type, for example), and the operator then works on it. This is the only form of operator overloading.
- `/`, `//`, `%` and `**` are defined directly on the built-in number types in the prototype, because their result types differ between `int` and `float`. Trait forms can be added if generic code needs them.
- `+` on `str` and `list` is concatenation. It is built in and is not part of `AddNumber`.

Trait names are `CamelCase` like every other type name, so the trait is spelled `AddNumber`.

### Shareable

`Shareable` marks the types that are safe to hand to another task: immutable values, `@frozen` structs of shareable fields, and the synchronised types (`Mutex`, `Atomic`, `Channel`). The compiler computes it for every type; it cannot be implemented or derived by hand. A trait that must be usable across tasks says so in its header, `trait Store(Shareable):`, which obliges every implementor to qualify. The full rules and the places they are enforced are in [04](04-runtime-and-memory.md#data-race-prevention).

## Type checker implementation notes

- **Type representation.** Types are interned in an arena and referred to by a small integer id. Equality is id comparison. Constructors: primitive, struct, enum, tuple, function, trait object, type parameter, inference variable, error.
- **Error type.** A failed expression gets the error type, which unifies with anything and suppresses follow-on errors.
- **Passes.**
  1. Collect all type and trait names in every module.
  2. Resolve signatures: struct fields, function signatures, trait methods, impl headers.
  3. Check coherence and that every impl is complete.
  4. Check function bodies, one function at a time, producing HIR.
  5. Run derivations (they produce HIR bodies that are checked like user code).
- **Trait lookup.** A table keyed by (trait, type constructor) holds impls. Resolving `T: Trait` for a concrete type is a table lookup plus recursive checks on the impl's own bounds. With no blanket impls and no associated types, this needs no general constraint solver.
- **Exhaustiveness.** `match` is checked with the standard pattern-matrix algorithm; the same algorithm produces the decision tree used by codegen.
- **Definite assignment.** A forward pass over the structured body tracks the set of definitely assigned variables through branches and loops.
- **Result usage.** An expression statement whose type is `Result` is an error.
