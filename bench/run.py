"""Run each benchmark in bench/ as a compiled program and with CPython, and print a table.

Usage: python3 bench/run.py [--runs N]

Each program is compiled once with `--release`. Both versions must print the same output.
Reported time is the best wall-clock time of N runs; memory is the peak resident set size.
"""

import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BENCH = ROOT / "bench"
COMPILER = ROOT / "target" / "release" / "weby"


def measure(command: list[str], runs: int) -> tuple[float, int, str]:
    best, peak, output = float("inf"), 0, ""
    for _ in range(runs):
        start = time.perf_counter()
        process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        stdout = process.stdout.read()
        _, status, usage = os.wait4(process.pid, 0)
        elapsed = time.perf_counter() - start
        if os.waitstatus_to_exitcode(status) != 0:
            sys.exit(f"{command[0]} failed with status {status}")
        best = min(best, elapsed)
        peak = max(peak, usage.ru_maxrss)
        output = stdout.decode()
    return best, peak, output


def main() -> None:
    runs = int(sys.argv[sys.argv.index("--runs") + 1]) if "--runs" in sys.argv else 3
    if not COMPILER.exists():
        sys.exit("build the compiler first: cargo build --release --workspace")
    print(f"| Benchmark | Compiled | CPython {sys.version.split()[0]} | Speed-up | Memory (compiled / CPython) |")
    print("|---|---|---|---|---|")
    with tempfile.TemporaryDirectory() as out:
        for source in sorted(BENCH.glob("*.wb")):
            executable = Path(out) / source.stem
            subprocess.run([COMPILER, "build", "--release", source, "-o", executable], check=True)
            compiled, compiled_mem, compiled_out = measure([str(executable)], runs)
            python, python_mem, python_out = measure([sys.executable, str(source.with_suffix(".py"))], runs)
            if compiled_out != python_out:
                sys.exit(f"{source.name}: outputs differ\n--- compiled\n{compiled_out}--- python\n{python_out}")
            print(
                f"| {source.stem} | {compiled:.3f} s | {python:.3f} s | {python / compiled:.1f}x "
                f"| {compiled_mem / 1024:.0f} MB / {python_mem / 1024:.0f} MB |"
            )


if __name__ == "__main__":
    main()
