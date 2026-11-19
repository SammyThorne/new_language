"""Run the HTTP benchmark: each server (Weby, Go, Python) under the same load on each endpoint.

Usage: python3 bench/http/run.py [--seconds N] [--connections N[,N...]] [--runs N] [--no-pin] [--browser]

For each endpoint and connection count, each server is started fresh and loaded `--runs` times
(default 3); the run with the median throughput is reported, with its median and 99th-percentile
latency and the server's peak resident memory during that run. It then measures each server's
startup time (from exec to the first answered request), its memory when idle, and the size of its
executable.

The load generator (load.go) runs on the same machine. Unless `--no-pin` is given, the server is
pinned to three physical cores (CPUs 0-2 and their SMT siblings 6-8) and the load generator to
the other three, so the two do not compete for the same cores; each server sees six CPUs.

With `--browser`, every request carries the dozen headers a browser sends (load.go, --browser)
instead of only Host.
"""

import os
import shutil
import socket
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
COMPILER = ROOT / "target" / "release" / "weby"
SERVER_CPUS = "0-2,6-8"
LOAD_CPUS = "3-5,9-11"

ORDER = (
    '{"customer": "ada@example.org", "lines": ['
    + ", ".join(f'{{"sku": "SKU-{i}", "qty": {i + 1}, "price": {i * 2.5 + 0.99}}}' for i in range(5))
    + "]}"
)

ENDPOINTS = [
    ("/plaintext", []),
    ("/json", []),
    ("/items", []),
    ("/users/42", []),
    ("/orders", ["POST", ORDER]),
]


def arg(name: str, default: str) -> str:
    return sys.argv[sys.argv.index(name) + 1] if name in sys.argv else default


def find_go() -> str:
    return shutil.which("go") or str(Path.home() / ".local" / "go" / "bin" / "go")


def rss_kib(pid: int) -> int:
    try:
        for line in Path(f"/proc/{pid}/status").read_text().splitlines():
            if line.startswith("VmRSS:"):
                return int(line.split()[1])
    except FileNotFoundError:
        pass
    return 0


def pinned(command: list[str], cpus: str) -> list[str]:
    return ["taskset", "-c", cpus, *command] if PIN else command


def start(command: list[str], port: int) -> subprocess.Popen:
    process = subprocess.Popen(pinned(command + [str(port)], SERVER_CPUS), stderr=subprocess.PIPE, stdout=subprocess.DEVNULL)
    process.stderr.readline()
    time.sleep(0.2)
    return process


def stop(process: subprocess.Popen) -> None:
    process.terminate()
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()


def load(out: Path, port: int, path: str, extra: list[str], connections: str, seconds: str, pid: int):
    """One load run: (requests/s, p50 ms, p99 ms, errors, peak RSS KiB)."""
    peak = [rss_kib(pid)]
    done = threading.Event()

    def sample():
        while not done.wait(0.05):
            peak[0] = max(peak[0], rss_kib(pid))

    sampler = threading.Thread(target=sample)
    sampler.start()
    try:
        fields = subprocess.run(
            pinned([str(out / "load"), *BROWSER, str(port), path, connections, seconds, *extra], LOAD_CPUS),
            capture_output=True,
            text=True,
            check=True,
        ).stdout.split()
    finally:
        done.set()
        sampler.join()
    return float(fields[0]), float(fields[1]), float(fields[2]), int(fields[3]), peak[0]


def startup(command: list[str], port: int) -> tuple[float, int]:
    """Seconds from exec until a request is answered, and resident memory (KiB) once idle."""
    began = time.perf_counter()
    process = subprocess.Popen(pinned(command + [str(port)], SERVER_CPUS), stderr=subprocess.DEVNULL, stdout=subprocess.DEVNULL)
    try:
        while True:
            try:
                with socket.create_connection(("127.0.0.1", port), timeout=1) as s:
                    s.sendall(b"GET /plaintext HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
                    if s.recv(64).startswith(b"HTTP/1.1 200"):
                        break
            except OSError:
                time.sleep(0.001)
        took = time.perf_counter() - began
        time.sleep(0.5)
        return took, rss_kib(process.pid)
    finally:
        stop(process)


def main() -> None:
    seconds = arg("--seconds", "5")
    counts = arg("--connections", "64").split(",")
    runs = int(arg("--runs", "3"))
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
        print(f"{seconds} s per run, median of {runs} runs; " + ("server and load generator pinned to separate cores" if PIN else "not pinned"))
        port = 19100
        for connections in counts:
            print()
            print(f"**{connections} connections.** Requests per second; median and p99 latency in ms; peak server memory.")
            print()
            print("| Endpoint | Weby | Go | Python | Weby vs Go | Weby vs Python |")
            print("|---|---|---|---|---|---|")
            for path, extra in ENDPOINTS:
                results = {}
                for name, command in servers.items():
                    port += 1
                    process = start(command, port)
                    try:
                        attempts = [load(out, port, path, extra, connections, seconds, process.pid) for _ in range(runs)]
                    finally:
                        stop(process)
                    attempts.sort(key=lambda r: r[0])
                    results[name] = attempts[len(attempts) // 2]
                    if any(r[3] for r in attempts):
                        print(f"<!-- {name} {path}: {sum(r[3] for r in attempts)} errors -->")
                cell = lambda r: f"{r[0]:,.0f} ({r[1]:.2f} / {r[2]:.2f}; {r[4] / 1024:.0f} MB)"
                weby, go_r, py = results["Weby"], results["Go"], results["Python"]
                label = f"`POST {path}`" if extra else f"`{path}`"
                print(
                    f"| {label} | {cell(weby)} | {cell(go_r)} | {cell(py)} | {weby[0] / go_r[0]:.2f}x | {weby[0] / py[0]:.1f}x |",
                    flush=True,
                )
        print()
        print("| Server | Startup to first response | Idle memory | Executable |")
        print("|---|---|---|---|")
        for name, command in servers.items():
            port += 1
            took = statistics.median([startup(command, port + i * 100)[0] for i in range(5)])
            _, idle = startup(command, port + 1000)
            size = Path(command[0]).stat().st_size if name != "Python" else 0
            shown = f"{size / 1e6:.1f} MB" if size else "(interpreter)"
            print(f"| {name} | {took * 1000:.0f} ms | {idle / 1024:.1f} MB | {shown} |")


PIN = "--no-pin" not in sys.argv
BROWSER = ["--browser"] if "--browser" in sys.argv else []
if __name__ == "__main__":
    main()
