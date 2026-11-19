// A sampling profiler to load with LD_PRELOAD, for machines without `perf`.
//
// Every 250 us of process CPU time (in practice every scheduler tick), SIGPROF interrupts
// whichever thread is running and records its instruction pointer, up to three callers found
// through frame pointers (generated code and the runtime keep them), the two words at the
// stack pointer (the caller of a leaf function without a frame, such as a system call stub),
// and whether the thread is the main thread. At exit, the samples and the process's memory
// map are written to the file named by SAMPLER_OUT, for report.py.
//
//   cc -O2 -shared -fPIC bench/profile/sampler.c -o sampler.so
//   SAMPLER_OUT=out.txt LD_PRELOAD=./sampler.so ./program
//   python3 bench/profile/report.py out.txt [more runs...]
#define _GNU_SOURCE
#include <signal.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <ucontext.h>
#include <unistd.h>

#define MAX_SAMPLES (1 << 21)
// Instruction pointer, three callers, then the two words at the stack pointer.
#define WORDS 6

static unsigned long samples[MAX_SAMPLES][WORDS];
static unsigned char on_main[MAX_SAMPLES];
static atomic_long count;

static void on_sample(int sig, siginfo_t *info, void *context) {
  (void)sig;
  (void)info;
  long i = atomic_fetch_add(&count, 1);
  if (i >= MAX_SAMPLES) return;
  ucontext_t *uc = context;
  unsigned long rip = uc->uc_mcontext.gregs[REG_RIP];
  unsigned long rbp = uc->uc_mcontext.gregs[REG_RBP];
  unsigned long rsp = uc->uc_mcontext.gregs[REG_RSP];
  unsigned long *row = samples[i];
  row[0] = rip;
  // Follow frame pointers only while they stay above the stack pointer and within 8 MiB.
  for (int d = 1; d < 4; d++) {
    row[d] = 0;
    if (rbp < rsp || rbp - rsp > (8ul << 20) || (rbp & 7)) continue;
    unsigned long *frame = (unsigned long *)rbp;
    row[d] = frame[1];
    if (frame[0] <= rbp) {
      for (int e = d + 1; e < 4; e++) row[e] = 0;
      break;
    }
    rbp = frame[0];
  }
  unsigned long *sp = (unsigned long *)rsp;
  row[4] = sp[0];
  row[5] = sp[1];
  on_main[i] = syscall(SYS_gettid) == getpid();
}

static void dump(void) {
  const char *out = getenv("SAMPLER_OUT");
  if (!out) return;
  struct itimerval off = {0};
  setitimer(ITIMER_PROF, &off, 0);
  FILE *f = fopen(out, "w");
  if (!f) return;
  FILE *maps = fopen("/proc/self/maps", "r");
  char line[512];
  while (maps && fgets(line, sizeof line, maps)) fprintf(f, "M %s", line);
  if (maps) fclose(maps);
  long n = count < MAX_SAMPLES ? count : MAX_SAMPLES;
  for (long i = 0; i < n; i++) {
    fprintf(f, "S %d", on_main[i]);
    for (int w = 0; w < WORDS; w++) fprintf(f, " %lx", samples[i][w]);
    fprintf(f, "\n");
  }
  fclose(f);
}

__attribute__((constructor)) static void start(void) {
  if (!getenv("SAMPLER_OUT")) return;
  struct sigaction action;
  memset(&action, 0, sizeof action);
  action.sa_sigaction = on_sample;
  action.sa_flags = SA_SIGINFO | SA_RESTART;
  sigaction(SIGPROF, &action, 0);
  struct itimerval every = {{0, 250}, {0, 250}};
  setitimer(ITIMER_PROF, &every, 0);
  atexit(dump);
}
