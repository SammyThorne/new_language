"""Run each benchmark in bench/ compiled with Weby, in Go, and in CPython, and print a table.

Usage: python3 bench/run.py [--runs N]

The Weby program is compiled with `--release`; the Go program with `go build`. Go is found on
PATH or in ~/.local/go/bin, and its column is left out if it is not installed. Every version must
print the same output. Reported time is the best wall-clock time of N runs; memory is the peak
resident set size.
"""

import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BENCH = ROOT / "bench"
COMPILER = ROOT / "target" / "release" / "weby"


def find_go() -> str | None:
    found = shutil.which("go")
    if found:
        return found
    local = Path.home() / ".local" / "go" / "bin" / "go"
    return str(local) if local.exists() else None


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


def version(command: list[str]) -> str:
    return subprocess.run(command, capture_output=True, text=True).stdout.split()[2]


def main() -> None:
    runs = int(sys.argv[sys.argv.index("--runs") + 1]) if "--runs" in sys.argv else 3
    if not COMPILER.exists():
        sys.exit("build the compiler first: cargo build --release --workspace")
    go = find_go()
    python = sys.version.split()[0]
    env = dict(os.environ, GOTOOLCHAIN="local")

    if go:
        go_version = version([go, "version"])
        print(f"| Benchmark | Weby | Go {go_version[2:]} | CPython {python} | Weby vs Go | Weby vs CPython | Memory: Weby / Go / CPython |")
        print("|---|---|---|---|---|---|---|")
    else:
        print(f"| Benchmark | Weby | CPython {python} | Weby vs CPython | Memory: Weby / CPython |")
        print("|---|---|---|---|---|")

    with tempfile.TemporaryDirectory() as out:
        for source in sorted(BENCH.glob("*.wb")):
            executable = Path(out) / source.stem
            subprocess.run([COMPILER, "build", "--release", source, "-o", executable], check=True)
            weby, weby_mem, weby_out = measure([str(executable)], runs)
            cpython, cpython_mem, cpython_out = measure([sys.executable, str(source.with_suffix(".py"))], runs)
            outputs = {"Weby": weby_out, "CPython": cpython_out}
            if go:
                go_executable = Path(out) / f"{source.stem}_go"
                subprocess.run([go, "build", "-o", go_executable, source.with_suffix(".go")], check=True, env=env)
                go_time, go_mem, go_out = measure([str(go_executable)], runs)
                outputs["Go"] = go_out
            if len(set(outputs.values())) != 1:
                details = "".join(f"--- {name}\n{text}" for name, text in outputs.items())
                sys.exit(f"{source.name}: outputs differ\n{details}")

            mb = lambda kb: f"{kb / 1024:.0f}"  # noqa: E731
            if go:
                ratio = weby / go_time
                vs_go = f"{ratio:.2f}x the time" if ratio >= 1 else f"{1 / ratio:.2f}x faster"
                print(
                    f"| {source.stem} | {weby:.3f} s | {go_time:.3f} s | {cpython:.3f} s | {vs_go} "
                    f"| {cpython / weby:.1f}x faster | {mb(weby_mem)} / {mb(go_mem)} / {mb(cpython_mem)} MB |"
                )
            else:
                print(
                    f"| {source.stem} | {weby:.3f} s | {cpython:.3f} s | {cpython / weby:.1f}x faster "
                    f"| {mb(weby_mem)} / {mb(cpython_mem)} MB |"
                )


if __name__ == "__main__":
    main()
