#!/usr/bin/env python3
"""Compare Viper's HTTP and WebSocket server with Go, Node.js and Python frameworks.

Every server answers the same REST endpoints (bench/http's service: /json, /users/{id}, /items,
POST /orders) and a WebSocket echo at /ws, and runs with the same CPU budget: it is pinned to
SERVER_CPUS cores and runs as many workers as it has cores, as each is deployed in production
(Go and Viper schedule across cores themselves; Node runs a node:cluster process per core;
Uvicorn and Gunicorn run a worker process per core). The load generator (load/main.go) is pinned
to the other cores.

Memory is the proportional set size (PSS) summed over the server's whole process tree, so
processes that share pages (forked workers) are not counted twice.

Usage: python3 bench/compare/run.py [--only NAME] [--quick]
Writes bench/compare/results.json.
"""

import argparse
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import threading
import time
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
SCRATCH = Path(os.environ.get("COMPARE_BUILD", "/tmp/viper-compare-build"))
CPUS = os.cpu_count() or 8
SERVER_CPUS = 4
SERVER_SET = f"0-{SERVER_CPUS - 1}"
LOAD_SET = f"{SERVER_CPUS}-{CPUS - 1}"
GO = shutil.which("go") or str(Path.home() / ".local" / "go" / "bin" / "go")

ORDER = (
    '{"customer": "ada@example.org", "lines": ['
    + ", ".join(f'{{"sku": "SKU-{i}", "qty": {i + 1}, "price": {i * 2.5 + 0.99}}}' for i in range(5))
    + "]}"
)
ENDPOINTS = [
    ("GET /json", "GET", "/json", ""),
    ("GET /users/{id}", "GET", "/users/42", ""),
    ("GET /items", "GET", "/items", ""),
    ("POST /orders", "POST", "/orders", ORDER),
]
VENV = HERE / "python" / ".venv" / "bin"
RATES = [1000, 10000, 50000, 100000, 200000, 400000]


def servers(port):
    """Name, command, working directory, environment, and whether it has /ws."""
    workers = str(SERVER_CPUS)
    return [
        ("Viper", [str(SCRATCH / "viper_server"), port], ROOT, {}, True),
        ("Go net/http", [str(SCRATCH / "go_server"), port], ROOT, {}, True),
        ("Node Fastify", ["node", "fastify.js", port], HERE / "node", {"WORKERS": workers}, True),
        ("Node Express", ["node", "express.js", port], HERE / "node", {"WORKERS": workers}, True),
        ("Python FastAPI", [str(VENV / "uvicorn"), "fastapi_app:app", "--host", "127.0.0.1", "--port", port,
                            "--workers", workers, "--loop", "uvloop", "--http", "httptools",
                            "--ws", "websockets", "--log-level", "warning", "--no-access-log"],
         HERE / "python", {}, True),
        ("Python Flask", [str(VENV / "gunicorn"), "-k", "gthread", "-w", workers, "--threads", "16",
                          "-b", f"127.0.0.1:{port}", "--log-level", "warning", "flask_app:app"],
         HERE / "python", {}, False),
    ]


def build():
    SCRATCH.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, GOTOOLCHAIN="local")
    subprocess.run([str(ROOT / "target" / "release" / "viper"), "build", "--release",
                    str(HERE / "viper" / "server.vi"), "-o", str(SCRATCH / "viper_server")], check=True, cwd=ROOT)
    subprocess.run([GO, "build", "-o", str(SCRATCH / "go_server"), "."], check=True, cwd=HERE / "go", env=env)
    subprocess.run([GO, "build", "-o", str(SCRATCH / "load"), "."], check=True, cwd=HERE / "load", env=env)


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def tree(pid):
    """pid and all its descendants."""
    children = {}
    for entry in os.listdir("/proc"):
        if entry.isdigit():
            try:
                with open(f"/proc/{entry}/stat") as f:
                    parent = int(f.read().rsplit(")", 1)[1].split()[1])
                children.setdefault(parent, []).append(int(entry))
            except (OSError, IndexError, ValueError):
                pass
    out, todo = [], [pid]
    while todo:
        p = todo.pop()
        out.append(p)
        todo.extend(children.get(p, []))
    return out


TICK = os.sysconf("SC_CLK_TCK")


def cpu_seconds(pid):
    """User plus system CPU time used so far by the process tree."""
    total = 0
    for p in tree(pid):
        try:
            with open(f"/proc/{p}/stat") as f:
                fields = f.read().rsplit(")", 1)[1].split()
            total += int(fields[11]) + int(fields[12])
        except (OSError, IndexError, ValueError):
            pass
    return total / TICK


def pss_mb(pid):
    total = 0
    for p in tree(pid):
        try:
            with open(f"/proc/{p}/smaps_rollup") as f:
                for line in f:
                    if line.startswith("Pss:"):
                        total += int(line.split()[1])
                        break
        except OSError:
            pass
    return total / 1024


class Sampler:
    """Samples the tree's memory every 100 ms while a phase runs, keeping the peak."""

    def __init__(self, pid):
        self.pid, self.peak, self.running = pid, 0.0, False

    def __enter__(self):
        self.peak, self.running = pss_mb(self.pid), True
        self.thread = threading.Thread(target=self.loop, daemon=True)
        self.thread.start()
        return self

    def loop(self):
        while self.running:
            self.peak = max(self.peak, pss_mb(self.pid))
            time.sleep(0.1)

    def __exit__(self, *exc):
        self.running = False
        self.thread.join()


def load(*args, timeout=180):
    out = subprocess.run(["taskset", "-c", LOAD_SET, str(SCRATCH / "load"), *map(str, args)],
                         capture_output=True, text=True, timeout=timeout)
    if out.returncode != 0:
        raise RuntimeError(out.stderr)
    return json.loads(out.stdout)


def wait_ready(port, proc, seconds=30):
    end = time.time() + seconds
    while time.time() < end:
        if proc.poll() is not None:
            raise RuntimeError(f"server exited with {proc.returncode}")
        try:
            with urllib.request.urlopen(f"http://127.0.0.1:{port}/json", timeout=1) as r:
                if r.status == 200:
                    return
        except OSError:
            time.sleep(0.1)
    raise RuntimeError("server did not start")


def check_answers(port):
    """Every server must give the same answers before it is measured."""
    with urllib.request.urlopen(f"http://127.0.0.1:{port}/users/42") as r:
        got = json.loads(r.read())
    assert got == {"id": 42, "name": "user 42", "email": "user42@example.com", "active": True}, got
    with urllib.request.urlopen(f"http://127.0.0.1:{port}/items") as r:
        assert len(json.loads(r.read())) == 20
    request = urllib.request.Request(f"http://127.0.0.1:{port}/orders", data=ORDER.encode(),
                                     headers={"Content-Type": "application/json"}, method="POST")
    with urllib.request.urlopen(request) as r:
        receipt = json.loads(r.read())
    assert receipt["count"] == 15 and abs(receipt["total"] - 114.85) < 1e-6, receipt


def start(command, cwd, env, port):
    proc = subprocess.Popen(["taskset", "-c", SERVER_SET, *command], cwd=cwd, env=dict(os.environ, **env),
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
    try:
        wait_ready(port, proc)
    except Exception:
        stop(proc)
        raise
    return proc


def stop(proc):
    try:
        os.killpg(proc.pid, signal.SIGTERM)
        proc.wait(timeout=10)
    except Exception:
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        proc.wait()


def measure(name, command, cwd, env, has_ws, port, quick):
    seconds = 2 if quick else 5
    result = {"name": name}
    proc = start(command, cwd, env, port)
    try:
        check_answers(port)
        time.sleep(1)
        result["idle_mb"] = pss_mb(proc.pid)
        result["processes"] = len(tree(proc.pid))
        load("http", port, "GET", "/json", 64, 3)  # warm up (Node's JIT, Python's caches)

        print(f"  {name}: endpoints", flush=True)
        result["endpoints"] = {}
        with Sampler(proc.pid) as sampler:
            for label, method, path, body in ENDPOINTS:
                cpu = cpu_seconds(proc.pid)
                got = load("http", port, method, path, 256, seconds, 0, body)
                got["cpu_seconds"] = cpu_seconds(proc.pid) - cpu
                got["cores"] = got["cpu_seconds"] / got["seconds"]
                got["per_cpu_second"] = got["count"] / got["cpu_seconds"] if got["cpu_seconds"] else None
                result["endpoints"][label] = got
        result["endpoints_peak_mb"] = sampler.peak

        print(f"  {name}: connections", flush=True)
        result["scaling"] = {}
        with Sampler(proc.pid) as sampler:
            for conns in [1, 16, 64, 256, 1024]:
                result["scaling"][conns] = load("http", port, "GET", "/users/42", conns, seconds)
        result["scaling_peak_mb"] = sampler.peak

        print(f"  {name}: fixed rates", flush=True)
        result["rates"] = {}
        for rate in RATES:
            got = load("http", port, "GET", "/users/42", 256, seconds + 3, rate)
            result["rates"][rate] = got
            if got["rate"] < 0.95 * rate or got["errors"]:
                break  # it could not keep up, so higher rates would only measure the backlog

        if has_ws:
            print(f"  {name}: websocket echo", flush=True)
            with Sampler(proc.pid) as sampler:
                cpu = cpu_seconds(proc.pid)
                result["ws_echo"] = load("ws-echo", port, 256, seconds, 64)
                result["ws_echo"]["cpu_seconds"] = cpu_seconds(proc.pid) - cpu
            result["ws_echo_peak_mb"] = sampler.peak
        result["final_mb"] = pss_mb(proc.pid)

        if has_ws:
            # A fresh server, so the heap grown by the load above does not hide what connections cost.
            print(f"  {name}: idle websockets", flush=True)
            stop(proc)
            time.sleep(1)
            proc = start(command, cwd, env, port)
            count = 2000 if quick else 10000
            time.sleep(2)
            before = pss_mb(proc.pid)
            holder = subprocess.Popen(["taskset", "-c", LOAD_SET, str(SCRATCH / "load"), "ws-idle", port, str(count)],
                                      stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
            line = holder.stdout.readline().split()
            held = int(line[1]) if len(line) > 1 else 0
            time.sleep(2)
            after = pss_mb(proc.pid)
            holder.stdin.close()
            holder.wait(timeout=60)
            result["ws_idle"] = {"connections": held, "failed": count - held, "before_mb": before, "after_mb": after,
                                 "kb_per_connection": (after - before) * 1024 / held if held else None}
    except Exception as error:  # record and go on to the next server
        result["error"] = str(error)
        print(f"  {name}: FAILED {error}", flush=True)
    finally:
        stop(proc)
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--only", help="run only servers whose name contains this text")
    parser.add_argument("--quick", action="store_true", help="short runs, to check the setup")
    options = parser.parse_args()
    build()
    out_path = HERE / ("results-quick.json" if options.quick else "results.json")
    machine = {
        "cpus": CPUS, "server_cpus": SERVER_CPUS, "kernel": os.uname().release,
        "cpu": next((l.split(":", 1)[1].strip() for l in open("/proc/cpuinfo") if l.startswith("model name")), "?"),
        "node": subprocess.run(["node", "--version"], capture_output=True, text=True).stdout.strip(),
        "go": subprocess.run([GO, "version"], capture_output=True, text=True).stdout.split()[2],
        "python": sys.version.split()[0],
        "date": time.strftime("%Y-%m-%d"),
    }
    results = []
    for name, *rest in servers("0"):
        if options.only and options.only.lower() not in name.lower():
            continue
        port = str(free_port())
        spec = next(s for s in servers(port) if s[0] == name)
        print(name, flush=True)
        results.append(measure(*spec, port, options.quick))
        out_path.write_text(json.dumps({"machine": machine, "results": results}, indent=1))
        time.sleep(2)
    print(f"wrote {out_path}")


if __name__ == "__main__":
    main()
