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
| `dispatch` | Dynamic dispatch: 10 million method calls through trait objects (Go interfaces, Python methods) |
| `fib` | Function calls and integer arithmetic: naive recursive `fib(35)` |
| `sieve` | List indexing and loops: a sieve of Eratosthenes over 20 million booleans, grown by appending |
| `binary_trees` | Allocation and garbage collection: the classic binary-trees benchmark at depth 18 |
| `word_count` | Strings and dicts: split, lower-case and count 20.5 million words |

The Go versions are written the way a Go programmer would write them. Where that changes the work done, the comment in the file says so. For example, `counts[key] = counts[key] + 1` is one map access in Go but two lookups in the other versions.

## Results

### 2026-11-18, end of milestone 3

AMD Ryzen AI 5 340 (12 threads), Debian 13, LLVM 19.1.7, Boehm GC 8.2.8, Go 1.27.4, CPython 3.13.5. Best of 5 runs, wall-clock time; memory is peak resident set size.

| Benchmark | Weby | Go 1.27.4 | CPython 3.13.5 | Weby vs Go | Weby vs CPython | Memory: Weby / Go / CPython |
|---|---|---|---|---|---|---|
| binary_trees | 0.957 s | 0.781 s | 20.309 s | 1.22x the time | 21.2x faster | 65 / 39 / 56 MB |
| dispatch | 0.046 s | 0.026 s | 0.505 s | 1.74x the time | 11.1x faster | 13 / 13 / 13 MB |
| fib | 0.017 s | 0.030 s | 0.607 s | 1.76x faster | 35.3x faster | 13 / 13 / 13 MB |
| sieve | 0.067 s | 0.053 s | 1.834 s | 1.28x the time | 27.2x faster | 54 / 62 / 161 MB |
| word_count | 1.077 s | 0.685 s | 2.007 s | 1.57x the time | 1.9x faster | 13 / 13 / 13 MB |

Milestone 3 did not change the four earlier benchmarks beyond run-to-run noise.

**Against the targets in docs/07** ("Targets", end-of-prototype column), every target is already met:

| Group | Target vs Go | Now |
|---|---|---|
| CPU (`fib`) | within 1.3x | faster than Go |
| Allocation (`binary_trees`) | within 3x | 1.22x, inside even the post-tuning target of 1.5x |
| Collections and strings (`sieve`, `word_count`) | within 2x | 1.28x and 1.57x |

docs/07 has no numeric target for dispatch; it is measured to compare trait objects with the generics that arrive in milestone 4.

**Gate 1** (docs/06-roadmap.md, "Gates") asks whether allocation-heavy code is at least 10x faster than CPython and whether the GC stress mode is clean. Both hold: `binary_trees` is 21x faster, and every test program passes with a collection before every allocation.

Notes:

- `fib` flatters the compiler: LLVM turns part of the recursion into a loop, as it would for C or Rust. Go's compiler does not.
- `binary_trees` uses more memory than Go (65 MB against 38 MB). Each object carries an 8-byte descriptor header, and the conservative collector grows the heap more eagerly than Go's.
- `dispatch` costs about 4.6 ns per call against Go's 2.6 ns. Both load a function pointer from a table and call it; the gap is unexplained so far and worth a profile once debug info exists. Generics (milestone 4) will give a static-dispatch alternative.
- `sieve` trails Go because every `append` is a call into the runtime, whereas Go inlines the common case. The optimisation backlog in docs/07 covers this: collections rewritten in the language, so the fast path inlines.
- `word_count` is the weakest against CPython. A string-keyed dict lookup costs about 20 ns, against 4 ns for an int key. The idiom `counts[k] = counts.get_or(k, 0) + 1` does two lookups, where Go does one, and `split()` builds a new list each time. CPython caches each string's hash, which offsets its interpreter overhead on this workload. The planned fixes are on the same backlog: inlinable collections, and an entry API that updates a value with one lookup.
