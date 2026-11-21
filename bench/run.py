"""Run each benchmark in bench/ compiled with Viper, in Go, and in CPython, and print a table.

Usage: python3 bench/run.py [--runs N] [--record | --check]

The Viper program is compiled with `--release`; the Go program with `go build`. Go is found on
PATH or in ~/.local/go/bin, and its column is left out if it is not installed. Every version must
print the same output. Reported time is the best wall-clock time of N runs; memory is the peak
resident set size.

`--record` saves each program's time, its time relative to Go's, and its peak memory in
bench/baseline.json. `--check` then runs only the Viper and Go versions and fails if any program's
time relative to Go's has grown by more than 15% (and by more than 5 ms), or its memory by
more than 10% (docs/10, milestone 5). Comparing with Go's time cancels most of this machine's
drift of up to 10% between runs; without Go, the plain times are compared.
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BENCH = ROOT / "bench"
COMPILER = ROOT / "target" / "release" / "viper"
BASELINE = BENCH / "baseline.json"
TIME_TOLERANCE = 0.15
MEMORY_TOLERANCE = 0.10
MIN_REGRESSION = 0.005


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


def check(results: dict[str, dict]) -> None:
    """Compare `results` with the recorded baseline; exit with status 1 on a regression."""
    baseline = json.loads(BASELINE.read_text())
    failures = []
    print()
    print("| Benchmark | Recorded | Now | Change | Memory recorded / now |")
    print("|---|---|---|---|---|")
    for name, now in results.items():
        old = baseline.get(name)
        if old is None:
            print(f"| {name} | (not recorded) | | | |")
            continue
        relative = "vs_go" in now and "vs_go" in old
        before, after = (old["vs_go"], now["vs_go"]) if relative else (old["time"], now["time"])
        change = after / before - 1
        memory_change = now["memory_kb"] / old["memory_kb"] - 1
        unit = "x Go's time" if relative else " s"
        print(
            f"| {name} | {before:.3f}{unit} | {after:.3f}{unit} | {change:+.1%} "
            f"| {old['memory_kb'] / 1024:.0f} / {now['memory_kb'] / 1024:.0f} MB |"
        )
        # Programs that run for a few milliseconds vary by more than the tolerance on their own.
        if change > TIME_TOLERANCE and now["time"] - old["time"] * (1 + TIME_TOLERANCE) > MIN_REGRESSION:
            failures.append(f"{name}: {change:+.1%} time")
        if memory_change > MEMORY_TOLERANCE:
            failures.append(f"{name}: {memory_change:+.1%} memory")
    if failures:
        sys.exit("regressions beyond the tolerance: " + "; ".join(failures))
    print()
    print(f"no regressions (tolerance {TIME_TOLERANCE:.0%} time, {MEMORY_TOLERANCE:.0%} memory)")


def main() -> None:
    runs = int(sys.argv[sys.argv.index("--runs") + 1]) if "--runs" in sys.argv else 3
    checking = "--check" in sys.argv
    results: dict[str, dict] = {}
    if not COMPILER.exists():
        sys.exit("build the compiler first: cargo build --release --workspace")
    go = find_go()
    python = sys.version.split()[0]
    env = dict(os.environ, GOTOOLCHAIN="local")

    if checking:
        pass
    elif go:
        go_version = version([go, "version"])
        print(f"| Benchmark | Viper | Go {go_version[2:]} | CPython {python} | Viper vs Go | Viper vs CPython | Memory: Viper / Go / CPython |")
        print("|---|---|---|---|---|---|---|")
    else:
        print(f"| Benchmark | Viper | CPython {python} | Viper vs CPython | Memory: Viper / CPython |")
        print("|---|---|---|---|---|")

    with tempfile.TemporaryDirectory() as out:
        for source in sorted(BENCH.glob("*.vi")):
            executable = Path(out) / source.stem
            subprocess.run([COMPILER, "build", "--release", source, "-o", executable], check=True)
            viper, viper_mem, viper_out = measure([str(executable)], runs)
            outputs = {"Viper": viper_out}
            cpython, cpython_mem = 0.0, 0
            if not checking:
                cpython, cpython_mem, outputs["CPython"] = measure([sys.executable, str(source.with_suffix(".py"))], runs)
            if go:
                go_executable = Path(out) / f"{source.stem}_go"
                subprocess.run([go, "build", "-o", go_executable, source.with_suffix(".go")], check=True, env=env)
                go_time, go_mem, go_out = measure([str(go_executable)], runs)
                outputs["Go"] = go_out
            if len(set(outputs.values())) != 1:
                details = "".join(f"--- {name}\n{text}" for name, text in outputs.items())
                sys.exit(f"{source.name}: outputs differ\n{details}")

            results[source.stem] = {"time": viper, "memory_kb": viper_mem}
            if go:
                results[source.stem]["vs_go"] = viper / go_time
            if checking:
                continue
            mb = lambda kb: f"{kb / 1024:.0f}"  # noqa: E731
            if go:
                ratio = viper / go_time
                vs_go = f"{ratio:.2f}x the time" if ratio >= 1 else f"{1 / ratio:.2f}x faster"
                print(
                    f"| {source.stem} | {viper:.3f} s | {go_time:.3f} s | {cpython:.3f} s | {vs_go} "
                    f"| {cpython / viper:.1f}x faster | {mb(viper_mem)} / {mb(go_mem)} / {mb(cpython_mem)} MB |"
                )
            else:
                print(
                    f"| {source.stem} | {viper:.3f} s | {cpython:.3f} s | {cpython / viper:.1f}x faster "
                    f"| {mb(viper_mem)} / {mb(cpython_mem)} MB |"
                )
    if "--record" in sys.argv:
        BASELINE.write_text(json.dumps(results, indent=2, sort_keys=True) + "\n")
        print(f"recorded {BASELINE.relative_to(ROOT)}")
    if checking:
        check(results)


if __name__ == "__main__":
    main()
