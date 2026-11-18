# Benchmarks

Each benchmark exists as a Weby program (`.wb`), a Go program (`.go`) and a CPython program (`.py`), all printing the same output. `run.py` builds the Weby version with `--release` and the Go version with `go build`, runs all three, checks that the outputs match, and prints a table. Go is found on `PATH` or in `~/.local/go/bin`; without it, the Go column is left out.

```bash
cargo build --release --workspace
```

```bash
python3 bench/run.py --runs 5
```

| Benchmark | What it measures |
|---|---|
| `fib` | Function calls and integer arithmetic: naive recursive `fib(35)` |
| `sieve` | List indexing and loops: a sieve of Eratosthenes over 20 million booleans, grown by appending |
| `binary_trees` | Allocation and garbage collection: the classic binary-trees benchmark at depth 18 |
| `word_count` | Strings and dicts: split, lower-case and count 20.5 million words |

The Go versions are written the way a Go programmer would write them. Where that changes the work done, the comment in the file says so. For example, `counts[key] = counts[key] + 1` is one map access in Go but two lookups in the other versions.

## Results

### 2026-11-18, end of milestone 2

AMD Ryzen AI 5 340 (12 threads), Debian 13, LLVM 19.1.7, Boehm GC 8.2.8, Go 1.27.4, CPython 3.13.5. Best of 5 runs, wall-clock time; memory is peak resident set size.

| Benchmark | Weby | Go 1.27.4 | CPython 3.13.5 | Weby vs Go | Weby vs CPython | Memory: Weby / Go / CPython |
|---|---|---|---|---|---|---|
| binary_trees | 0.988 s | 0.773 s | 19.822 s | 1.28x the time | 20.1x faster | 65 / 38 / 57 MB |
| fib | 0.017 s | 0.031 s | 0.607 s | 1.80x faster | 35.7x faster | 13 / 13 / 13 MB |
| sieve | 0.079 s | 0.054 s | 1.841 s | 1.46x the time | 23.3x faster | 53 / 54 / 162 MB |
| word_count | 1.082 s | 0.718 s | 2.035 s | 1.51x the time | 1.9x faster | 13 / 13 / 13 MB |

**Against the targets in docs/07** ("Targets", end-of-prototype column), every target is already met:

| Group | Target vs Go | Now |
|---|---|---|
| CPU (`fib`) | within 1.3x | faster than Go |
| Allocation (`binary_trees`) | within 3x | 1.28x, inside even the post-tuning target of 1.5x |
| Collections and strings (`sieve`, `word_count`) | within 2x | 1.46x and 1.51x |

**Gate 1** (docs/06-roadmap.md, "Gates") asks whether allocation-heavy code is at least 10x faster than CPython and whether the GC stress mode is clean. Both hold: `binary_trees` is 20x faster, and all 20 test programs pass with a collection before every allocation.

Notes:

- `fib` flatters the compiler: LLVM turns part of the recursion into a loop, as it would for C or Rust. Go's compiler does not.
- `binary_trees` uses more memory than Go (65 MB against 38 MB). Each object carries an 8-byte descriptor header, and the conservative collector grows the heap more eagerly than Go's.
- `sieve` trails Go because every `append` is a call into the runtime, whereas Go inlines the common case. The optimisation backlog in docs/07 covers this: collections rewritten in the language, so the fast path inlines.
- `word_count` is the weakest against CPython. A string-keyed dict lookup costs about 20 ns, against 4 ns for an int key. The idiom `counts[k] = counts.get_or(k, 0) + 1` does two lookups, where Go does one, and `split()` builds a new list each time. CPython caches each string's hash, which offsets its interpreter overhead on this workload. The planned fixes are on the same backlog: inlinable collections, and an entry API that updates a value with one lookup.
