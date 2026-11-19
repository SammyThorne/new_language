// Counts the calls a program makes to the system-call wrappers and allocators that decide a
// server's per-request cost, and prints the totals to standard error when the program exits.
// Divide them by the number of requests to get calls per request; bench/http/counts.py does this
// for every endpoint.
//
//   cc -O2 -shared -fPIC bench/profile/count.c -o count.so -ldl
//   LD_PRELOAD=./count.so ./server 8080
//
// Counted: recv (and how many failed, which on a non-blocking socket means "nothing to read"),
// send, writev, epoll_wait (and how many returned nothing), read and write (the scheduler's
// eventfd), clock_gettime, the Rust heap (malloc, calloc and realloc together, and free), and the
// collector's entry points. Allocations that generated code makes inline never call the collector
// and are not counted; each GC_malloc_many call refills one of their free lists.
//
// The shim wraps malloc, so it slows allocation-heavy programs: use it for counts, never rates.
#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdatomic.h>
#include <stdio.h>
#include <sys/epoll.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <sys/uio.h>
#include <time.h>
#include <unistd.h>

extern void *__libc_malloc(size_t);
extern void *__libc_calloc(size_t, size_t);
extern void *__libc_realloc(void *, size_t);
extern void __libc_free(void *);

#define COUNTER(name) static atomic_long n_##name
COUNTER(recv); COUNTER(recv_empty); COUNTER(send); COUNTER(writev); COUNTER(epoll); COUNTER(epoll_empty);
COUNTER(read); COUNTER(write); COUNTER(clock); COUNTER(malloc); COUNTER(free);
COUNTER(gc_malloc); COUNTER(gc_atomic); COUNTER(gc_many);

#define NEXT(ret, name, ...) static ret (*real)(__VA_ARGS__); if (!real) real = dlsym(RTLD_NEXT, name)

void *malloc(size_t n) { n_malloc++; return __libc_malloc(n); }
void *calloc(size_t a, size_t b) { n_malloc++; return __libc_calloc(a, b); }
void *realloc(void *p, size_t n) { n_malloc++; return __libc_realloc(p, n); }
void free(void *p) { if (p) n_free++; __libc_free(p); }

ssize_t recv(int fd, void *b, size_t n, int f) {
  NEXT(ssize_t, "recv", int, void *, size_t, int);
  ssize_t r = real(fd, b, n, f); n_recv++; if (r < 0) n_recv_empty++; return r;
}
ssize_t send(int fd, const void *b, size_t n, int f) {
  NEXT(ssize_t, "send", int, const void *, size_t, int); n_send++; return real(fd, b, n, f);
}
ssize_t writev(int fd, const struct iovec *v, int c) {
  NEXT(ssize_t, "writev", int, const struct iovec *, int); n_writev++; return real(fd, v, c);
}
int epoll_wait(int e, struct epoll_event *ev, int m, int t) {
  NEXT(int, "epoll_wait", int, struct epoll_event *, int, int);
  int r = real(e, ev, m, t); n_epoll++; if (r <= 0) n_epoll_empty++; return r;
}
int clock_gettime(clockid_t c, struct timespec *t) {
  NEXT(int, "clock_gettime", clockid_t, struct timespec *); n_clock++; return real(c, t);
}
ssize_t write(int fd, const void *b, size_t n) {
  NEXT(ssize_t, "write", int, const void *, size_t); n_write++; return real(fd, b, n);
}
ssize_t read(int fd, void *b, size_t n) {
  NEXT(ssize_t, "read", int, void *, size_t); n_read++; return real(fd, b, n);
}
void *GC_malloc(size_t n) { NEXT(void *, "GC_malloc", size_t); n_gc_malloc++; return real(n); }
void *GC_malloc_atomic(size_t n) { NEXT(void *, "GC_malloc_atomic", size_t); n_gc_atomic++; return real(n); }
void *GC_malloc_many(size_t n) { NEXT(void *, "GC_malloc_many", size_t); n_gc_many++; return real(n); }
void GC_generic_malloc_many(size_t n, int k, void **r) {
  NEXT(void, "GC_generic_malloc_many", size_t, int, void **); n_gc_many++; real(n, k, r);
}

__attribute__((destructor)) static void report(void) {
  fprintf(stderr,
          "COUNT recv=%ld recv_empty=%ld send=%ld writev=%ld epoll_wait=%ld epoll_empty=%ld read=%ld write=%ld "
          "clock_gettime=%ld malloc=%ld free=%ld GC_malloc=%ld GC_malloc_atomic=%ld GC_malloc_many=%ld\n",
          (long)n_recv, (long)n_recv_empty, (long)n_send, (long)n_writev, (long)n_epoll, (long)n_epoll_empty,
          (long)n_read, (long)n_write, (long)n_clock, (long)n_malloc, (long)n_free, (long)n_gc_malloc,
          (long)n_gc_atomic, (long)n_gc_many);
}
