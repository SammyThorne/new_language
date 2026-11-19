"""Calls and allocation per request, for each endpoint of the Weby server: system calls, clock
reads, Rust heap allocations, collector calls and bytes allocated. Unlike rates, these counts are
exact and repeatable, so they show whether a change did what it was meant to.

Usage: python3 bench/http/counts.py [--server EXECUTABLE] [--seconds 3] [--connections 256] [--browser]

The server runs with bench/profile/count.c preloaded and `WEBY_ALLOC_STATS=1`, pinned like
run.py. The shim slows allocation, so the rates it prints are not comparable with run.py's.
"""

import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

from run import COMPILER, ENDPOINTS, HERE, LOAD_CPUS, arg, find_go, start, stop

COLUMNS = [
    ("recv", "recv"),
    ("recv_empty", "failing"),
    ("send", "send"),
    ("epoll_wait", "epoll_wait"),
    ("clock_gettime", "clock"),
    ("malloc", "malloc"),
    ("GC_malloc", "GC_malloc"),
    ("GC_malloc_atomic", "GC_malloc_atomic"),
    ("GC_malloc_many", "refills"),
    ("bytes", "bytes allocated"),
]


def main() -> None:
    seconds = arg("--seconds", "3")
    connections = arg("--connections", "256")
    browser = ["--browser"] if "--browser" in sys.argv else []
    env = dict(os.environ, GOTOOLCHAIN="local")
    with tempfile.TemporaryDirectory() as out:
        out = Path(out)
        server = arg("--server", "")
        if not server:
            server = str(out / "server")
            subprocess.run([COMPILER, "build", "--release", HERE / "server.wb", "-o", server], check=True)
        subprocess.run([find_go(), "build", "-o", out / "load", HERE / "load.go"], check=True, env=env)
        shim = out / "count.so"
        subprocess.run(["cc", "-O2", "-shared", "-fPIC", HERE.parent / "profile" / "count.c", "-o", shim, "-ldl"], check=True)
        print(f"Per request, {connections} connections, {seconds} s" + (", browser headers" if browser else ""))
        print()
        print("| Endpoint | " + " | ".join(title for _, title in COLUMNS) + " |")
        print("|---|" + "---|" * len(COLUMNS))
        port = 19800
        saved = dict(os.environ)
        for path, extra in ENDPOINTS:
            port += 1
            os.environ.update({"LD_PRELOAD": str(shim), "WEBY_ALLOC_STATS": "1"})
            try:
                process = start([server], port)
            finally:
                os.environ.clear()
                os.environ.update(saved)
            try:
                fields = subprocess.run(
                    ["taskset", "-c", LOAD_CPUS, str(out / "load"), *browser, str(port), path, connections, seconds, *extra],
                    capture_output=True, text=True, check=True,
                ).stdout.split()
            finally:
                stop(process)
            report = process.stderr.read().decode()
            counts = {key: int(value) for key, value in re.findall(r"(\w+)=(\d+)", report)}
            requests = float(fields[0]) * float(seconds)
            label = f"`POST {path}`" if extra else f"`{path}`"
            cells = [f"{counts.get(key, 0) / requests:,.2f}" if key != "bytes" else f"{counts.get(key, 0) / requests:,.0f}" for key, _ in COLUMNS]
            print(f"| {label} | " + " | ".join(cells) + " |", flush=True)


if __name__ == "__main__":
    main()
