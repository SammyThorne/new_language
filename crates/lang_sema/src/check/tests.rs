use super::*;
use lang_diag::SourceMap;

fn check_text(text: &str) -> (Option<hir::Program>, Vec<&'static str>) {
    let (program, found) = check_messages(text);
    (program, found.into_iter().map(|(code, _)| code).collect())
}

fn check_messages(text: &str) -> (Option<hir::Program>, Vec<(&'static str, String)>) {
    let mut sources = SourceMap::default();
    let file = sources.add("test.src", text);
    let mut diags = Diagnostics::default();
    let tokens = lang_syntax::lex(file, text, &mut diags);
    let module = lang_syntax::parse(&tokens, &mut diags);
    assert!(!diags.has_errors(), "test program does not parse");
    let program = check(&module, &mut sources, "test", &mut diags);
    (program, diags.sorted().iter().map(|d| (d.code, d.message.clone())).collect())
}

fn ok(text: &str) -> hir::Program {
    let (program, found) = check_messages(text);
    assert!(found.is_empty(), "unexpected diagnostics {found:?}");
    program.unwrap()
}

fn errors(text: &str) -> Vec<&'static str> {
    check_text(text).1
}

#[test]
fn hello_world_checks() {
    let program = ok("def main():\n    print(\"Hello, world!\")\n");
    assert_eq!(program.functions[program.entry.0].name, "main");
}

#[test]
fn locals_take_the_type_of_their_first_assignment() {
    ok("def main():\n    x = 1\n    x = x + 2\n    print(x)\n");
    assert_eq!(errors("def main():\n    x = 1\n    x = \"one\"\n    print(x)\n"), [codes::TYPE_MISMATCH]);
}

#[test]
fn definite_assignment_follows_branches() {
    ok(
        "def f(c: bool):\n    if c:\n        x = 1\n    else:\n        x = 2\n    print(x)\n\ndef main():\n    f(True)\n",
    );
    assert_eq!(
        errors("def f(c: bool):\n    if c:\n        x = 1\n    print(x)\n\ndef main():\n    f(True)\n"),
        [codes::USED_BEFORE_ASSIGNED]
    );
    ok(
        "def f(c: bool):\n    if c:\n        x = 1\n    else:\n        return\n    print(x)\n\ndef main():\n    f(True)\n",
    );
    assert_eq!(
        errors("def main():\n    for i in range(3):\n        last = i\n    print(last)\n"),
        [codes::USED_BEFORE_ASSIGNED]
    );
}

#[test]
fn missing_return_understands_branches_and_infinite_loops() {
    ok(
        "def f(c: bool) -> int:\n    if c:\n        return 1\n    else:\n        return 2\n\ndef main():\n    print(f(True))\n",
    );
    ok("def f() -> int:\n    while True:\n        return 1\n\ndef main():\n    print(f())\n");
    assert_eq!(
        errors("def f(c: bool) -> int:\n    if c:\n        return 1\n\ndef main():\n    print(f(True))\n"),
        [codes::MISSING_RETURN]
    );
    ok("def f() -> int:\n    panic(\"no\")\n\ndef main():\n    print(f())\n");
}

#[test]
fn int_literals_adapt_to_float() {
    ok("def main():\n    x = 2.5\n    y = x * 2\n    z: float = 1\n    print(y + z, 1 + x)\n");
    assert_eq!(errors("def main():\n    n = 2\n    x = 2.5\n    print(n * x)\n"), [codes::BAD_OPERANDS]);
}

#[test]
fn structs_and_methods() {
    ok("\
struct Point:
    x: int
    y: int = 0

impl Point:
    def origin() -> Point:
        return Point(0, 0)

    def moved(self, dx: int) -> Point:
        return Point(x=self.x + dx, y=self.y)

def main():
    p = Point.origin().moved(3)
    p.y = 4
    print(p.x, p.y)
");
    assert_eq!(errors("struct P:\n    x: int\n\ndef main():\n    p = P()\n    print(p.x)\n"), [codes::MISSING_FIELD]);
    assert_eq!(errors("struct P:\n    x: int\n\ndef main():\n    p = P(1)\n    print(p.z)\n"), [codes::NO_SUCH_MEMBER]);
}

#[test]
fn collections_and_options() {
    ok("\
def main():
    words = [\"a\", \"b\"]
    counts: dict[str, int] = {}
    for w in words:
        counts[w] = counts.get_or(w, 0) + 1
    first: str? = None
    if len(words) > 0:
        first = words[0]
    for k, v in counts.items():
        print(k, v, first.unwrap_or(\"\"))
    pair = (1, \"x\")
    n, s = pair
    print(n, s, pair[1])
");
    assert_eq!(errors("def main():\n    xs = []\n    print(xs)\n"), [codes::CANNOT_INFER]);
    assert_eq!(errors("def main():\n    d: dict[float, int] = {}\n    print(d)\n"), [codes::UNHASHABLE_KEY]);
}

#[test]
fn conditions_must_be_bool() {
    assert_eq!(errors("def main():\n    n = 1\n    if n:\n        print(n)\n"), [codes::NOT_BOOL]);
}

#[test]
fn errors_do_not_cascade() {
    assert_eq!(errors("def main():\n    x = undefined\n    y = x + 1\n    print(y + x)\n"), [codes::UNKNOWN_NAME]);
}

#[test]
fn format_specs_are_checked() {
    ok("def main():\n    x = 3.14159\n    print(f\"{x:.2f} {1:>4}\")\n");
    assert_eq!(errors("def main():\n    print(f\"{'a':.2f}\")\n"), [codes::BAD_FORMAT_SPEC]);
}

const SHAPES: &str = "\
trait Shape:
    def area(self) -> float
    def name(self) -> str

    def describe(self) -> str:
        return f\"{self.name()} with area {self.area():.1f}\"

struct Circle:
    radius: float

impl Shape for Circle:
    def area(self) -> float:
        return 3.0 * self.radius * self.radius

    def name(self) -> str:
        return \"circle\"

def total(shapes: list[Shape]) -> float:
    sum = 0.0
    for s in shapes:
        sum += s.area()
    return sum
";

#[test]
fn traits_defaults_and_trait_objects() {
    let program =
        ok(&format!("{SHAPES}\ndef main():\n    c = Circle(1.0)\n    print(c.describe(), total([c, Circle(2.0)]))\n"));
    // The default `describe` is copied into Circle's implementation.
    assert!(program.functions.iter().any(|f| f.name == "Circle.describe"));
    assert_eq!(
        errors(&format!(
            "{SHAPES}\nstruct Square:\n    side: float\n\nimpl Shape for Square:\n    def area(self) -> float:\n        return self.side\n\ndef main():\n    print(1)\n"
        )),
        [codes::BAD_IMPL]
    );
    assert_eq!(
        errors(&format!(
            "{SHAPES}\nstruct Square:\n    side: float\n\nimpl Shape for Square:\n    def area(self) -> int:\n        return 1\n\n    def name(self) -> str:\n        return \"s\"\n\ndef main():\n    print(1)\n"
        )),
        [codes::BAD_IMPL]
    );
    // A default method can only use the trait's own methods on `self`.
    assert_eq!(
        errors(
            "trait T:\n    def f(self) -> int\n\n    def g(self) -> int:\n        return self.missing\n\ndef main():\n    print(1)\n"
        ),
        [codes::NO_SUCH_MEMBER]
    );
    assert_eq!(errors("def f(x: Eq):\n    print(1)\n\ndef main():\n    print(1)\n"), [codes::NOT_OBJECT_SAFE]);
}

#[test]
fn enums_and_match() {
    ok("\
@derive(Eq, Display)
enum Shape:
    Circle(radius: float)
    Square(side: float)
    Empty

def area(s: Shape) -> float:
    match s:
        case Circle(r):
            return 3.0 * r * r
        case Shape.Square(side):
            return side * side
        case Empty:
            return 0.0

def main():
    print(area(Circle(1.0)), area(Shape.Square(2.0)), area(Shape.Empty), Empty == Shape.Empty)
");
    let non_exhaustive = "\
enum Color:
    Red
    Green
    Blue

def main():
    c = Color.Red
    match c:
        case Red:
            print(1)
        case Green:
            print(2)
";
    assert_eq!(errors(non_exhaustive), [codes::NON_EXHAUSTIVE]);
    let unreachable = "def main():\n    x = 3\n    match x:\n        case _:\n            print(1)\n        case 3:\n            print(2)\n";
    assert_eq!(errors(unreachable), [codes::UNREACHABLE_CASE]);
    ok(
        "def main():\n    x: int? = 3\n    match x:\n        case Some(n) if n > 2:\n            print(n)\n        case Some(_) | None:\n            print(0)\n",
    );
    assert_eq!(
        errors("def main():\n    b = True\n    match b:\n        case True:\n            print(1)\n"),
        [codes::NON_EXHAUSTIVE]
    );
}

#[test]
fn results_and_question_mark() {
    ok("\
struct NotFound:
    what: str

impl Error for NotFound:
    def message(self) -> str:
        return self.what + \" not found\"

def find(id: int) -> Result[int]:
    if id < 0:
        return Err(NotFound(\"user\"))
    return id * 2

def total(a: str, b: str) -> Result[int]:
    return int.parse(a)? + int.parse(b)? + find(1)?

def main():
    match total(\"1\", \"2\"):
        case Ok(n):
            print(n)
        case Err(e):
            print(e.message())
    _ = find(3)
");
    assert_eq!(errors("def f() -> Result[int]:\n    return 1\n\ndef main():\n    f()\n"), [codes::UNUSED_RESULT]);
    ok(
        "def save(n: int) -> Result[None]:\n    if n < 0:\n        return\n    print(n)\n\ndef main():\n    _ = save(1)\n",
    );
    assert_eq!(errors("def main():\n    n = int.parse(\"1\")?\n    print(n)\n"), [codes::BAD_TRY]);
}

#[test]
fn derives_and_frozen() {
    ok(
        "@derive(Eq, Hash, Display, Copy)\nstruct P:\n    x: int\n\ndef main():\n    d = {P(1): \"a\"}\n    print(d[P(1)], P(1) == P(1), P(2).copy())\n",
    );
    assert_eq!(
        errors("@derive(Hash)\nstruct P:\n    x: int\n\ndef main():\n    print(1)\n"),
        [codes::MISSING_REQUIRED_TRAIT]
    );
    assert_eq!(
        errors("@frozen\nstruct P:\n    x: int\n\ndef main():\n    p = P(1)\n    p.x = 2\n"),
        [codes::FROZEN_FIELD]
    );
    assert_eq!(errors("struct P:\n    x: int\n\ndef main():\n    print(P(1) == P(1))\n"), [codes::BAD_OPERANDS]);
}

#[test]
fn number_traits_give_operators() {
    ok("\
struct Money:
    cents: int

impl AddNumber for Money:
    def add(self, other: Money) -> Money:
        return Money(self.cents + other.cents)

def main():
    total = Money(5) + Money(7)
    print(total.cents)
");
}

#[test]
fn suggestions() {
    assert_eq!(closest("prnt", ["print", "main"].into_iter()), Some("print"));
    assert_eq!(closest("xyz", ["print"].into_iter()), None);
    // A swapped pair is one change, and case does not count.
    assert_eq!(closest("nmae", ["name", "age"].into_iter()), Some("name"));
    assert_eq!(closest("Strr", ["str", "int"].into_iter()), Some("str"));
    assert_eq!(closest("Name", ["name"].into_iter()), Some("name"));
    assert_eq!(edit_distance("kitten", "sitting"), 3);
}
