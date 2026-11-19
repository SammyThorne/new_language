# 04 — Runtime and memory

Every Weby executable links `liblang_rt.a`, a Rust static library exposing C-ABI functions prefixed `lrt_`. The runtime owns four things: memory, tasks, I/O, and the native parts of the standard library.

## Responsibilities

| Area | Contents |
|---|---|
| Startup | Initialise the collector and scheduler, call the program's `main`, flush output, exit |
| Memory | Allocation entry points, collector configuration, handle table |
| Core data | String operations, list growth, hash map, formatting of numbers |
| Panics | Message, source location, backtrace, task or process termination |
| Tasks | `spawn`, `join`, channels, mutexes, timers, sleep |
| I/O | Files, sockets, DNS, standard streams |
| Web | HTTP/1.1 connection handling, router, WebSocket framing, JSON scanner |

The runtime is built with `panic = "abort"`: a Rust panic inside the runtime is a runtime bug and must not unwind into Weby frames.

## Object model

Every heap object begins with a one-word header that points to a constant **type descriptor** emitted by the compiler.

| Descriptor field | Use |
|---|---|
| Size in bytes | Copying, debugging, future collectors |
| Pointer bitmap | Which words of the object hold references; unused by Boehm, required by a precise collector |
| Type id | Dynamic type tests on trait objects |
| Type name | Panic messages, debugging, `Display` derivation |

The header costs eight bytes per object. It is paid from the start because retrofitting it would change every layout in the compiler.

Layouts:

| Value | Layout |
|---|---|
| Struct | Header, then fields in declaration order with natural alignment |
| `str` / `bytes` | The value is a (pointer, length) pair. The bytes live in a pointer-free allocation with no header. String literals point into the executable's read-only data and are never collected |
| `list[T]` | Header, length, capacity, pointer to a separate element buffer. Elements are stored inline (a `list[int]` is a flat array of `i64`) |
| `dict[K, V]` | Header, then an open-addressing table plus an insertion-ordered entries array. Prototype: implemented in the runtime, parameterised by key and value size with hash and equality function pointers. Later: rewritten in Weby so it monomorphises and inlines |
| Closure environment | Header, then captured variables. A variable captured by a closure and mutated lives in the environment, not on the stack |
| Enum (boxed) | Header, tag, payload |

## Garbage collection

### Phase 1: Boehm–Demers–Weiser (bdwgc)

A conservative, mark-and-sweep collector. It finds roots by scanning thread stacks, registers and static data for anything that looks like a pointer into its heap.

What the compiler does: call the allocation function. Nothing else. No stack maps, no safepoint polls, no write barriers.

Configuration:

- Thread support and thread-local allocation enabled, so allocation in one task does not take a global lock.
- Parallel marking enabled.
- **Pointer-free allocation** for string bytes, `bytes`, and element buffers of lists whose element type contains no references. The collector neither scans these nor mistakes their contents for pointers. For a web server, where most of the heap volume is request and response bytes, this is the most important tuning decision.
- No finalisers. The language has none, and they slow collection.
- Initial heap size and growth factor exposed through environment variables (`WEBY_GC_INITIAL_HEAP`, `WEBY_GC_FREE_SPACE_DIVISOR`) for tuning without recompiling.
- Linking: the system's `libgc.so.1` (Debian package `libgc1`), linked by its full file name so that no development package is needed; the runtime declares the few functions it uses itself. Vendoring and static linking for releases is still the plan, so that binaries are self-contained and the build flags are ours (D21 in [08](08-decisions-and-risks.md)).
- Stress mode: `WEBY_GC_STRESS=1` collects before every allocation. The end-to-end tests run every program this way.

### The Rust heap rule

Boehm scans its own heap, thread stacks and static data. It does **not** scan memory obtained from Rust's allocator. A Weby object referenced only from a Rust `Vec`, `Box` or `HashMap` will be collected while still in use.

Rule: the runtime never stores a Weby reference in Rust-allocated memory. It may hold one in exactly three places:

1. On the stack of a running task, for the duration of a runtime call.
2. Inside another GC-allocated object.
3. In the **handle table**: a GC-visible, uncollectable array managed by one small module. Long-lived references (registered route handlers, a WebSocket connection's callback, a task's entry closure) are stored there and referred to from Rust by index.

Enforcement: Weby references cross the ABI as a distinct newtype that does not implement the traits needed to be stored in ordinary Rust collections; a GC stress mode (collect at every allocation) runs the whole test suite in CI and turns violations into immediate crashes.

### Beyond Boehm

Boehm's limits are known: no compaction, no generations, whole-heap pauses that grow with heap size, and occasional false retention. Whether they matter is an empirical question answered at the M8 gate ([06](06-roadmap.md#gates)). If they do, the path is:

1. **Reduce allocation** first. Escape analysis to stack-allocate non-escaping structs and closures; per-connection buffer reuse in the HTTP path; avoiding intermediate strings in JSON. This helps under any collector. Structs are done, along with an inline allocation fast path and pointer-free structs in unscanned memory ([08](08-decisions-and-risks.md), D83).
2. **Precise stack scanning.** Introduce the mid-level IR, then either LLVM's shadow-stack strategy (simple, slower) or statepoints (fast, complex) so the collector knows exactly which stack slots are references.
3. **A precise, moving collector.** The leading candidate is [MMTk](https://www.mmtk.io/), a Rust framework that provides production collectors (Immix, generational Immix) behind a binding interface. We would implement object scanning (using the pointer bitmaps we already emit) and root enumeration. Writing our own Go-style concurrent collector is a last resort.

Nothing in the language semantics changes at any step.

## Concurrency

### Model

The language has **tasks**. `spawn f(x)` creates one and returns a `Task[T]`. Tasks run concurrently and, with multiple cores, in parallel. Any operation that waits (socket read, channel receive, mutex lock, sleep, `join`) is a call into the runtime, and the runtime decides how to wait. User code contains no `await` and no callbacks.

This single indirection is what allows two implementations behind one language.

### Stage 1: one OS thread per task (prototype)

- `spawn` creates a native thread with a modest stack (configurable, default 256 KiB of committed-on-demand memory) and registers it with the collector.
- Waiting operations are ordinary blocking system calls and OS synchronisation primitives.
- The HTTP server accepts on one thread and hands each connection to a task. Keep-alive connections hold their thread while idle.
- A cap on live tasks protects against thread exhaustion; beyond it, `spawn` waits.

Good enough for: correctness, the whole prototype, thousands of concurrent connections, and a decisive win over Python frameworks. Not good enough for: tens of thousands of idle WebSocket connections, or matching Go under high connection counts, because of per-thread memory and kernel context switches.

### Stage 2: M:N fibers (milestone 10)

A task becomes a **fiber**: a user-space thread with its own stack, scheduled by the runtime onto a fixed pool of worker threads (default: one per core).

| Component | Design |
|---|---|
| Fiber stacks | Each is a virtual memory reservation (default 256 KiB, configurable) with a guard page below it; physical memory is committed on touch. Stack overflow hits the guard page and becomes a clean panic. Freed stacks are pooled |
| Context switch | A short assembly routine per architecture that saves callee-saved registers and swaps stack pointers. Either hand-written or taken from the `corosensei` crate, provided we keep control of stack allocation |
| Scheduler | Each worker has a local run queue; idle workers steal from others; a global queue receives tasks made runnable from outside a worker. This is the design Go and Tokio both use |
| Network poller | Sockets are non-blocking. A read that would block registers the fiber with epoll (edge-triggered) and switches away. A poller wakes fibers when their sockets are ready |
| Timers | A per-worker timer heap serves `sleep`, timeouts and deadlines |
| Blocking work | File I/O, DNS lookups and calls into C through `@extern` run on a separate pool of threads so they cannot stall a worker |
| Sync primitives | Channels and mutexes park and wake fibers, not threads |
| Fairness | Cooperative at first: fibers switch at waiting operations. A CPU-bound loop can hold a worker. Later: the compiler inserts a cheap check on loop back-edges and function entry so the scheduler can request a yield |

**Collector integration.** While Boehm is the collector, it must see every fiber's stack. Running fibers are on worker threads it already knows. For parked fibers, the runtime registers a callback (Boehm's "push other roots" hook) that reports the live region of every parked fiber stack; saved registers are on those stacks because the context switch pushes them. This is the mechanism Crystal uses. It is the reason we own fiber stack allocation and do not adopt an off-the-shelf coroutine runtime.

**As built in M10** ([08](08-decisions-and-risks.md), D61 to D66): as above, with these differences. Stacks reserve 1 MiB by default rather than 256 KiB (only touched pages cost memory). The scheduler's queues are locked deques rather than lock-free ones. Idle workers wait in `epoll_wait` on one shared poller instead of a separate poller thread, and one timer thread replaces per-worker timer heaps. There is no blocking pool yet, no yield check, and a stack overflow is a segmentation fault rather than a panic. Running fibers are made visible to the collector by moving the worker thread's registered stack bottom on every switch, under the collector's allocation lock (D62). The program's `main` stays on the main thread (D63).

**io_uring** is a possible later replacement for epoll. It is not planned: epoll is sufficient for Go parity and far simpler.

### Synchronisation primitives

| Primitive | Semantics |
|---|---|
| `Task[T]` | `join()` waits and returns `T`. Joining a panicked task panics in the joiner |
| `Channel[T](capacity)` | Multi-producer, multi-consumer. `send` waits when full; `recv` returns `T?` and yields `None` once closed and drained; iterable. Capacity 0 is a rendezvous |
| `Mutex[T]` | Wraps a value; `lock()` returns a `Scoped[T]` guard, so `with m.lock() as value:` gives access to the value and unlocks on exit. There is no way to reach the value without the lock |
| `RwLock[T]` | Many readers or one writer (M6 stretch) |
| `Atomic[int]`, `Atomic[bool]` | Lock-free counters and flags |
| `select` | Waiting on several channels at once. Deferred; needs syntax |

### Data-race prevention

A data race is two tasks touching the same memory at the same time, at least one of them writing, with no lock. In a language with multi-word values and growable collections a race is not only a logic bug: a reader can see a string pointer with another string's length, or a list whose length exceeds its buffer. Weby prevents the common cases at compile time by controlling what may be shared between tasks.

#### Shareable types

`Shareable` is a built-in marker trait. The compiler decides which types have it.

| Type | Shareable? |
|---|---|
| `int`, `float`, `bool`, `str`, `bytes`, `None`, sized numerics | Yes. Immutable values |
| `tuple[...]`, `T?`, `Result[T, E]`, enums | Yes, if every part is |
| `@frozen` struct | Yes, if every field's type is. Its fields cannot be reassigned after construction |
| `Mutex[T]`, `RwLock[T]`, `Atomic[T]`, `Channel[T]`, `Task[T]`, `http.WebSocket` | Yes. Synchronised internally |
| Function value | Yes, if everything it captures is shareable and it does not reassign a captured variable |
| Trait object | Yes, if the trait requires it: `trait Store(Shareable):` |
| Ordinary struct, `list`, `dict`, `set`, `bytearray` | No. Mutable |

#### Where it is enforced

| Place | Rule |
|---|---|
| `spawn f(args)` | `f` and every argument must be shareable |
| `Channel[T]` | `T` must be shareable |
| `Mutex[T]`, `RwLock[T]` | `T` must be shareable, or be one mutable layer over shareable contents: a `list`, `dict`, `set` or ordinary struct whose elements or fields are shareable |
| Route handlers and `server.mount(value)` | The handler or value must be shareable, because requests are served concurrently |
| `with lock as name:` | `name` may be read, mutated and passed as an argument. It may not be assigned to another variable or field, returned, sent, or captured by a closure or `spawn`. Shareable values taken out of it (a frozen `Todo` read from a guarded `dict`) may leave freely |
| `task.join()` | Unrestricted. Under the rules above a finished task cannot have left a second reference to a mutable result anywhere another task can reach |

What the error looks like:

```python
struct Counter:
    hits: int

def main():
    counter = Counter(hits=0)
    spawn bump(counter)     # error: `Counter` cannot be shared with another task
                            #        because its fields can be changed
                            # help:  wrap it in `Mutex(...)`, or mark the struct `@frozen`
```

#### What this guarantees and what it does not

Guaranteed: two tasks never hold the same unsynchronised mutable struct or collection by way of `spawn`, a channel, or a route handler. That removes the memory-corrupting class of races and the most common logic races.

Not guaranteed:

- **A lock-guarded value stored by a callee.** A function that is passed a guarded collection can store it inside another object, and that reference then outlives the lock. Closing this hole needs the compiler to track where every reference flows, which is the machinery of Rust's borrow checker, and the readability goal rules that out.
- **C code** reached through `@extern`.
- **Logic races**, such as checking a value under one lock acquisition and acting on it under another. No type system prevents these.

The backstop for what the static rules cannot see is a race-detecting build mode, `weby build --race`, which instruments memory accesses using LLVM's ThreadSanitizer support and reports races when they happen in tests. It is post-prototype, and how well it coexists with a conservative collector has to be confirmed.

#### What the rules cost the programmer

- Data shared between tasks is either frozen or behind a lock. Read-only shared collections therefore need immutable collection types (`tuple[T, ...]`, `frozenset[T]`, a frozen `dict`), which move up to M6 for this reason.
- A mutable value cannot be handed to another task even if the sender never touches it again. Recognising that as a safe "move" needs ownership tracking; it is an open question for after the prototype. The workaround is to build the value inside the task that uses it, or to make it frozen.

#### Implementation

`Shareable` is a recursive property of a type, computed and cached during type checking. Most enforcement points are ordinary trait bounds on built-in generic types (`Channel[T: Shareable]`), plus a check at `spawn` and a flag on names bound by `with` on a lock. There is no run-time cost.

## Panics

A panic means the program has a bug. It prints the message, the source location and a backtrace.

- **Stage 1.** The process exits with a non-zero status. A panicking request handler therefore takes the server down; a process supervisor restarts it. This is harsh and is accepted for the prototype because expected failures are `Result`s.
- **Stage 2.** A panic terminates only its task. Each task keeps a small stack of active `with` cleanups; on panic the runtime runs them (releasing locks, closing sockets), marks the task as panicked, discards its fiber stack and switches to the scheduler. No unwinding tables are needed because nothing below the fiber's entry point has to be resumed. The HTTP server turns a panicked handler into a 500 response and keeps serving.

As built in M10 (D64): a panic in a task unwinds, using unwind tables the compiler now emits, to the task's entry in the runtime. The runtime releases the mutexes the task held; other `with` cleanups (closing files) do not run yet, because generated code has no landing pads. Joining a panicked task panics in the joiner, and a panicking HTTP handler gets a 500 and its connection is closed. A panic in `main` ends the process with status 101.

Catching panics in user code is not provided.

## Runtime ABI

The contract between generated code and the runtime. Grouped; each group is a handful of functions.

| Group | Examples of operations |
|---|---|
| Memory | Allocate with descriptor; allocate pointer-free; handle table create/get/release |
| Panic | Panic with message and location; bounds, division-by-zero and unwrap failures as dedicated entry points to keep call sites small |
| Strings | Concatenate, compare, hash, slice, find, case conversion, builder append and finish, number parsing and formatting |
| Lists | Grow, insert, remove, sort with comparator |
| Dicts and sets | Create, get, insert, remove, iterate |
| Output | Print, write to standard streams |
| Tasks | Spawn with entry closure, join, yield, sleep, current task id |
| Sync | Channel create/send/recv/close; mutex create/lock/unlock; atomics |
| Time | Monotonic and wall clocks |
| Files and sockets | Open, read, write, close; listen, accept, connect |
| HTTP | Create server, add route with handler handle, listen; request accessors; response writer |
| WebSocket | Receive message, send text/binary, close |
| JSON | Scanner (next token, string unescape, number parse); writer (escape and append) |

Conventions:

- C calling convention, no Rust types in signatures: integers, floats, pointers and small `repr(C)` structs only.
- Strings cross as (pointer, length). The runtime never assumes a terminating zero.
- Fallible operations return a status and write their result through an out-pointer; the standard library wrapper turns that into a `Result`.
- The ABI is internal. It is versioned with the compiler and can change freely between releases, because the compiler and runtime always ship together.

## Calling back into Weby

The HTTP server, `spawn` and sort comparators need the runtime to call Weby code. A callback is a function value (code pointer and environment pointer). The runtime stores it in the handle table and invokes the code pointer with the environment and arguments using the C calling convention; the compiler emits a C-convention entry stub for every function whose value escapes to the runtime.

## Platform support

| Platform | Status | Platform-specific pieces |
|---|---|---|
| Linux x86-64 | Prototype target | epoll, context switch, linker flags |
| Linux aarch64 | First port | Context switch stub, target triple |
| macOS aarch64 | Second port | kqueue poller, linker flags, guard page details |
| Windows | Not planned | IOCP poller, different linker and C runtime |
