# Benchmarks

Each benchmark exists as a compiled program (`.wb`) and as an equivalent CPython program (`.py`) that prints the same output. `run.py` compiles each with `--release`, runs both, checks that the outputs match, and prints a table.

```bash
cargo build --release --workspace
```

```bash
python3 bench/run.py --runs 3
```

| Benchmark | What it measures |
|---|---|
| `fib` | Function calls and integer arithmetic: naive recursive `fib(35)` |
| `sieve` | List indexing and loops: a sieve of Eratosthenes over 20 million booleans |
| `binary_trees` | Allocation and garbage collection: the classic binary-trees benchmark at depth 18 |
| `word_count` | Strings and dicts: split, lower-case and count 20.5 million words |

Go baselines (docs/07-testing-and-performance.md) are not included yet because Go is not installed on the development machine.

## Results

### 2026-11-18, end of milestone 2

AMD Ryzen AI 5 340 (12 threads), Debian 13, LLVM 19.1.7, Boehm GC 8.2.8, CPython 3.13.5. Best of 3 runs, wall-clock time; memory is peak resident set size.

| Benchmark | Compiled | CPython 3.13.5 | Speed-up | Memory (compiled / CPython) |
|---|---|---|---|---|
| binary_trees | 0.974 s | 20.021 s | 20.6x | 65 MB / 56 MB |
| fib | 0.017 s | 0.607 s | 35.6x | 13 MB / 13 MB |
| sieve | 0.077 s | 1.869 s | 24.1x | 52 MB / 161 MB |
| word_count | 1.094 s | 2.035 s | 1.9x | 13 MB / 13 MB |

**Gate 1** (docs/06-roadmap.md, "Gates") asks whether allocation-heavy code is at least 10x faster than CPython and whether the GC stress mode is clean. Both hold: `binary_trees` is 20.6x faster, and all 20 test programs pass with a collection before every allocation.

Notes:

- `fib` flatters the compiler: LLVM turns part of the recursion into a loop, as it would for C or Rust.
- `word_count` is the weak spot. A string-keyed dict lookup costs about 20 ns (an int-keyed one about 4 ns). The idiom `counts[k] = counts.get_or(k, 0) + 1` does two lookups, and `split()` builds a new list every time. CPython caches each string's hash, which offsets its interpreter overhead on this workload. The planned fixes are on the optimisation backlog in docs/07: collections rewritten in the language so hashing and comparison inline, and an entry API that updates a value with one lookup.
