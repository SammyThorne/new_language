"""Compare two builds of the same program, A and B, by running them interleaved in alternating
order (A B, B A, A B, ...), so that the machine's drift over minutes affects both alike. This is
the only way to see effects of a few percent on the benchmark machine, whose speed varies by
about 10% between runs a few minutes apart.

Batch programs (the default): each run's wall time.

    python3 bench/ab.py A B [--rounds 5]

Servers built from bench/http/server.vi: requests per second on each endpoint of run.py.

    python3 bench/ab.py A B --server [--one-core] [--rounds 3] [--seconds 4] [--connections N]
                        [--endpoints /items,/plaintext] [--browser]

With `--one-core` each server gets one CPU and one worker (as one_core.py does) and the default is
64 connections; otherwise it gets the six CPUs run.py gives it and 256 connections. Builds of A
and B usually come from the same source compiled against two runtimes:
`VIPER_RUNTIME=old/liblang_rt.a viper build --release ...`.

Prints each side's median and range and B's change against A: for batch programs negative is
faster, for servers positive is faster.
"""

import os
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "http"))
from run import ENDPOINTS, HERE, LOAD_CPUS, SERVER_CPUS, arg, find_go  # noqa: E402


def order(rounds: int) -> list[str]:
    sides = []
    for i in range(rounds):
        sides += ["A", "B"] if i % 2 == 0 else ["B", "A"]
    return sides


def batch(programs: dict[str, str], rounds: int) -> None:
    times = {"A": [], "B": []}
    for side in order(rounds):
        began = time.perf_counter()
        subprocess.run(["taskset", "-c", SERVER_CPUS, programs[side]], stdout=subprocess.DEVNULL, check=True)
        times[side].append(time.perf_counter() - began)
    report("wall time (s)", times, lower_is_better=True)


def serve(programs: dict[str, str], rounds: int) -> None:
    seconds = arg("--seconds", "4")
    one_core = "--one-core" in sys.argv
    connections = arg("--connections", "64" if one_core else "256")
    browser = ["--browser"] if "--browser" in sys.argv else []
    wanted = arg("--endpoints", "")
    endpoints = [e for e in ENDPOINTS if not wanted or e[0] in wanted.split(",")]
    cpus = "0" if one_core else SERVER_CPUS
    env = dict(os.environ, **({"VIPER_WORKERS": "1"} if one_core else {}))
    with tempfile.TemporaryDirectory() as out:
        load = Path(out) / "load"
        subprocess.run([find_go(), "build", "-o", load, HERE / "load.go"], check=True, env=dict(os.environ, GOTOOLCHAIN="local"))
        port = 19500
        for path, extra in endpoints:
            rates = {"A": [], "B": []}
            for side in order(rounds):
                port += 1
                process = subprocess.Popen(
                    ["taskset", "-c", cpus, programs[side], str(port)], env=env, stderr=subprocess.PIPE, stdout=subprocess.DEVNULL
                )
                process.stderr.readline()
                time.sleep(0.2)
                try:
                    fields = subprocess.run(
                        ["taskset", "-c", LOAD_CPUS, str(load), *browser, str(port), path, connections, seconds, *extra],
                        capture_output=True, text=True, check=True,
                    ).stdout.split()
                finally:
                    process.terminate()
                    process.wait()
                rates[side].append(float(fields[0]))
            report(("POST " if extra else "") + path + " (req/s)", rates, lower_is_better=False)


def report(label: str, values: dict[str, list[float]], lower_is_better: bool) -> None:
    a, b = statistics.median(values["A"]), statistics.median(values["B"])
    change = (b / a - 1) * 100
    better = (change < 0) == lower_is_better
    shown = lambda v: f"{v:,.4g}" if v < 1000 else f"{v:,.0f}"
    spread = lambda side: f"{shown(min(values[side]))}-{shown(max(values[side]))}"
    print(
        f"{label}: A {shown(a)} [{spread('A')}]  B {shown(b)} [{spread('B')}]  "
        f"B {change:+.1f}% ({'better' if better else 'worse'})",
        flush=True,
    )


def main() -> None:
    positional = [a for a in sys.argv[1:] if not a.startswith("--")]
    flags_with_values = {"--rounds", "--seconds", "--connections", "--endpoints"}
    values = {sys.argv[i + 1] for i, a in enumerate(sys.argv[:-1]) if a in flags_with_values}
    programs = [p for p in positional if p not in values]
    if len(programs) != 2:
        sys.exit(__doc__)
    sides = {"A": str(Path(programs[0]).resolve()), "B": str(Path(programs[1]).resolve())}
    if "--server" in sys.argv:
        serve(sides, int(arg("--rounds", "3")))
    else:
        batch(sides, int(arg("--rounds", "5")))


if __name__ == "__main__":
    main()
