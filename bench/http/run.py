"""Run the HTTP benchmark: each server (Weby, Go, Python) under the same load on each endpoint.

Usage: python3 bench/http/run.py [--seconds N] [--connections N]

Prints a table of requests per second and median and 99th-percentile latency. The load
generator (load.go) runs on the same machine as the server.
"""

import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
COMPILER = ROOT / "target" / "release" / "weby"


def find_go() -> str:
    return shutil.which("go") or str(Path.home() / ".local" / "go" / "bin" / "go")


def start(command: list[str], port: int) -> subprocess.Popen:
    process = subprocess.Popen(command, stderr=subprocess.PIPE, stdout=subprocess.DEVNULL)
    process.stderr.readline()
    time.sleep(0.2)
    return process


def main() -> None:
    seconds = sys.argv[sys.argv.index("--seconds") + 1] if "--seconds" in sys.argv else "5"
    connections = sys.argv[sys.argv.index("--connections") + 1] if "--connections" in sys.argv else "64"
    go = find_go()
    env = dict(os.environ, GOTOOLCHAIN="local")
    with tempfile.TemporaryDirectory() as out:
        out = Path(out)
        subprocess.run([COMPILER, "build", "--release", HERE / "server.wb", "-o", out / "weby_server"], check=True)
        subprocess.run([go, "build", "-o", out / "go_server", HERE / "server.go"], check=True, env=env)
        subprocess.run([go, "build", "-o", out / "load", HERE / "load.go"], check=True, env=env)
        servers = {
            "Weby": [str(out / "weby_server")],
            "Go": [str(out / "go_server")],
            "Python": [sys.executable, str(HERE / "server.py")],
        }
        print(f"{connections} connections, {seconds} s per run; requests/s (median / p99 latency in ms)")
        print()
        print("| Endpoint | Weby | Go | Python | Weby vs Go | Weby vs Python |")
        print("|---|---|---|---|---|---|")
        port = 19100
        for path in ["/plaintext", "/json", "/items"]:
            results = {}
            for name, command in servers.items():
                port += 1
                process = start(command + [str(port)], port)
                try:
                    load = subprocess.run(
                        [out / "load", str(port), path, connections, seconds], capture_output=True, text=True, check=True
                    ).stdout.split()
                    results[name] = (float(load[0]), float(load[1]), float(load[2]), int(load[3]))
                finally:
                    process.terminate()
                    process.wait()
            cell = lambda r: f"{r[0]:,.0f} ({r[1]:.2f} / {r[2]:.2f})"
            weby, go_r, py = results["Weby"], results["Go"], results["Python"]
            print(
                f"| `{path}` | {cell(weby)} | {cell(go_r)} | {cell(py)} | {weby[0] / go_r[0]:.2f}x | {weby[0] / py[0]:.1f}x |"
            )


main()
