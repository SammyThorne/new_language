"""Calls and allocation per request, for each endpoint of the Viper server: system calls, clock
reads, Rust heap allocations, collector calls and bytes allocated. Unlike rates, these counts are
exact and repeatable, so they show whether a change did what it was meant to.

Usage: python3 bench/http/counts.py [--server EXECUTABLE] [--seconds 3] [--connections 256] [--browser]
                                     [--observed] [--record | --check]

The server runs with bench/profile/count.c preloaded and `VIPER_ALLOC_STATS=1`, pinned like
run.py. The shim slows allocation, so the rates it prints are not comparable with run.py's.

`--observed` runs the server as production would, with the access log and request metrics on
(`server.vi ... observed`), and compares with the plain counts in counts.json; its log goes to a
file, read afterwards for the counts. Its record is counts_observed.json (docs/14, milestone 8).

`--record` saves the counts in bench/http/counts.json (or counts_browser.json with --browser);
`--check` fails if any count that depends only on the code has grown: system calls, clock reads
and allocations by more than 0.1 per request or 5%, bytes allocated by more than 5%. `epoll_wait`
and refills depend on timing and are not checked (docs/10, milestone 5).
"""

import json
import os
import re
import subprocess
import sys
import tempfile
import time
from pathlib import Path

from run import COMPILER, ENDPOINTS, HERE, LOAD_CPUS, SERVER_CPUS, arg, find_go, pinned, start, stop

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


CHECKED = ["recv", "recv_empty", "send", "clock_gettime", "malloc", "GC_malloc", "GC_malloc_atomic", "bytes"]


def check(recorded: dict, now: dict) -> list[str]:
    failures = []
    for endpoint, counts in now.items():
        for key in CHECKED:
            before, after = recorded.get(endpoint, {}).get(key), counts.get(key, 0.0)
            if before is None:
                continue
            allowed = before * 1.05 if key == "bytes" else max(before + 0.1, before * 1.05)
            if after > allowed:
                failures.append(f"{endpoint} {key}: {before:.2f} -> {after:.2f}")
    return failures


def start_observed(server: str, port: int, log: Path) -> subprocess.Popen:
    """The server with its access log on, writing standard error to `log`: a pipe nobody reads
    while the load runs would fill and stop it."""
    with open(log, "wb") as sink:
        process = subprocess.Popen(pinned([server, str(port), "observed"], SERVER_CPUS), stderr=sink, stdout=subprocess.DEVNULL)
    deadline = time.monotonic() + 10
    while b"listening on" not in log.read_bytes():
        if time.monotonic() > deadline or process.poll() is not None:
            sys.exit(f"the observed server did not start: {log.read_text()}")
        time.sleep(0.01)
    time.sleep(0.2)
    return process


def tail(path: Path, size: int = 8192) -> str:
    with open(path, "rb") as f:
        f.seek(max(0, path.stat().st_size - size))
        return f.read().decode(errors="replace")


def main() -> None:
    seconds = arg("--seconds", "3")
    connections = arg("--connections", "256")
    browser = ["--browser"] if "--browser" in sys.argv else []
    observed = "--observed" in sys.argv
    env = dict(os.environ, GOTOOLCHAIN="local")
    with tempfile.TemporaryDirectory() as out:
        out = Path(out)
        server = arg("--server", "")
        if not server:
            server = str(out / "server")
            subprocess.run([COMPILER, "build", "--release", HERE / "server.vi", "-o", server], check=True)
        subprocess.run([find_go(), "build", "-o", out / "load", HERE / "load.go"], check=True, env=env)
        shim = out / "count.so"
        subprocess.run(["cc", "-O2", "-shared", "-fPIC", HERE.parent / "profile" / "count.c", "-o", shim, "-ldl"], check=True)
        print(f"Per request, {connections} connections, {seconds} s" + (", browser headers" if browser else "") + (", access log and metrics on" if observed else ""))
        print()
        print("| Endpoint | " + " | ".join(title for _, title in COLUMNS) + " |")
        print("|---|" + "---|" * len(COLUMNS))
        port = 19800
        saved = dict(os.environ)
        results: dict[str, dict[str, float]] = {}
        for path, extra in ENDPOINTS:
            port += 1
            os.environ.update({"LD_PRELOAD": str(shim), "VIPER_ALLOC_STATS": "1"})
            log = out / "server.log"
            try:
                process = start_observed(server, port, log) if observed else start([server], port)
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
            report = tail(log) if observed else process.stderr.read().decode()
            # The shim's line and the allocation statistics, not the access log's fields.
            report = "\n".join(line for line in report.splitlines() if line.startswith("COUNT ") or "bytes=" in line)
            counts = {key: int(value) for key, value in re.findall(r"(\w+)=(\d+)", report)}
            requests = float(fields[0]) * float(seconds)
            label = f"`POST {path}`" if extra else f"`{path}`"
            results[label] = {key: counts.get(key, 0) / requests for key, _ in COLUMNS}
            cells = [f"{counts.get(key, 0) / requests:,.2f}" if key != "bytes" else f"{counts.get(key, 0) / requests:,.0f}" for key, _ in COLUMNS]
            print(f"| {label} | " + " | ".join(cells) + " |", flush=True)
    record = HERE / ("counts_browser.json" if browser else "counts_observed.json" if observed else "counts.json")
    if observed:
        plain = json.loads((HERE / "counts.json").read_text())
        print()
        print("Added by the access log and metrics, per request:")
        print()
        print("| Endpoint | " + " | ".join(title for key, title in COLUMNS if key in CHECKED) + " |")
        print("|---|" + "---|" * len(CHECKED))
        for label, counts in results.items():
            cells = []
            for key, _ in COLUMNS:
                if key in CHECKED:
                    delta = counts[key] - plain.get(label, {}).get(key, 0.0)
                    cells.append(f"{delta:+,.0f}" if key == "bytes" else f"{delta:+,.2f}")
            print(f"| {label} | " + " | ".join(cells) + " |")
    if "--record" in sys.argv:
        record.write_text(json.dumps(results, indent=2, sort_keys=True) + "\n")
        print(f"recorded {record.name}")
    if "--check" in sys.argv:
        failures = check(json.loads(record.read_text()), results)
        if failures:
            sys.exit("counts grew: " + "; ".join(failures))
        print("no count grew")


if __name__ == "__main__":
    main()
