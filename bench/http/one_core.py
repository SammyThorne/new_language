"""Requests per second on one CPU: the Weby server with one worker and the Go server with
GOMAXPROCS=1, each pinned to CPU 0, against the same load generator on other CPUs. Dividing a
second by the rate gives the CPU time each request costs, which is the figure to size a
deployment by.

Usage: python3 bench/http/one_core.py [--seconds 4] [--runs 3] [--connections 64]
"""

import os
import subprocess
import tempfile
from pathlib import Path

from run import COMPILER, ENDPOINTS, HERE, LOAD_CPUS, arg, find_go, start, stop


def main() -> None:
    seconds = arg("--seconds", "4")
    runs = int(arg("--runs", "3"))
    connections = arg("--connections", "64")
    go = find_go()
    env = dict(os.environ, GOTOOLCHAIN="local")
    with tempfile.TemporaryDirectory() as out:
        out = Path(out)
        subprocess.run([COMPILER, "build", "--release", HERE / "server.wb", "-o", out / "weby_server"], check=True)
        subprocess.run([go, "build", "-o", out / "go_server", HERE / "server.go"], check=True, env=env)
        subprocess.run([go, "build", "-o", out / "load", HERE / "load.go"], check=True, env=env)
        servers = {
            "Weby": ([str(out / "weby_server")], {"WEBY_WORKERS": "1"}),
            "Go": ([str(out / "go_server")], {"GOMAXPROCS": "1"}),
        }
        print(f"One CPU per server, {connections} connections, best of {runs} runs of {seconds} s")
        print()
        print("| Endpoint | Weby | Go | Weby vs Go | CPU per request: Weby / Go |")
        print("|---|---|---|---|---|")
        port = 19700
        for path, extra in ENDPOINTS:
            rates = {}
            for name, (command, variables) in servers.items():
                best = 0.0
                for _ in range(runs):
                    port += 1
                    saved = dict(os.environ)
                    os.environ.update(variables)
                    try:
                        process = start(["taskset", "-c", "0", *command], port)
                    finally:
                        os.environ.clear()
                        os.environ.update(saved)
                    try:
                        fields = subprocess.run(
                            ["taskset", "-c", LOAD_CPUS, str(out / "load"), str(port), path, connections, seconds, *extra],
                            capture_output=True, text=True, check=True,
                        ).stdout.split()
                    finally:
                        stop(process)
                    best = max(best, float(fields[0]))
                rates[name] = best
            label = f"`POST {path}`" if extra else f"`{path}`"
            weby, go_rate = rates["Weby"], rates["Go"]
            print(
                f"| {label} | {weby:,.0f} | {go_rate:,.0f} | {weby / go_rate:.2f}x | "
                f"{1e6 / weby:.1f} / {1e6 / go_rate:.1f} µs |",
                flush=True,
            )


if __name__ == "__main__":
    main()
