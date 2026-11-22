"""What the router's linear scan costs: `GET /users/42` on one CPU, with 10 parameterised routes
registered before it (the usual benchmark server) and with 210 (`server.vi ... routes200`), and the
CPU time per request of each. Milestone 24 of docs/14 builds a tree of path segments if the 200
extra routes cost more than 5% of a request (with a linear scan they cost 23%; with the tree, 0.3%).

Usage: python3 bench/http/routes.py [--seconds 4] [--runs 5] [--connections 64]
"""

import os
import subprocess
import tempfile
import time
from pathlib import Path

from run import COMPILER, HERE, LOAD_CPUS, arg, find_go, stop


def main() -> None:
    seconds = arg("--seconds", "4")
    runs = int(arg("--runs", "5"))
    connections = arg("--connections", "64")
    go = find_go()
    env = dict(os.environ, GOTOOLCHAIN="local")
    with tempfile.TemporaryDirectory() as out:
        out = Path(out)
        subprocess.run([COMPILER, "build", "--release", HERE / "server.vi", "-o", out / "viper_server"], check=True)
        subprocess.run([go, "build", "-o", out / "load", HERE / "load.go"], check=True, env=env)
        best = {}
        port = 19800
        # Alternate the two, so that drift in the machine's speed affects both alike.
        for _ in range(runs):
            for label, mode in [("10 routes", ""), ("210 routes", "routes200")]:
                port += 1
                command = ["taskset", "-c", "0", str(out / "viper_server"), str(port)] + ([mode] if mode else [])
                environment = dict(os.environ, VIPER_WORKERS="1")
                process = subprocess.Popen(command, stderr=subprocess.PIPE, stdout=subprocess.DEVNULL, env=environment)
                process.stderr.readline()
                time.sleep(0.2)
                try:
                    fields = subprocess.run(
                        ["taskset", "-c", LOAD_CPUS, str(out / "load"), str(port), "/users/42", connections, seconds],
                        capture_output=True, text=True, check=True,
                    ).stdout.split()
                finally:
                    stop(process)
                best[label] = max(best.get(label, 0.0), float(fields[0]))
        print(f"`GET /users/42`, one CPU, {connections} connections, best of {runs} runs of {seconds} s")
        print()
        print("| Routes before it | Requests/s | CPU per request |")
        print("|---|---|---|")
        for label, rate in best.items():
            print(f"| {label} | {rate:,.0f} | {1e6 / rate:.2f} µs |")
        few, many = best["10 routes"], best["210 routes"]
        extra = 1e6 / many - 1e6 / few
        share = extra / (1e6 / many) * 100
        print()
        print(f"The 200 extra routes add {extra:.2f} µs per request, {share:.1f}% of it.")
        print("Over 5%: the router costs too much at this many routes." if share > 5.0 else "Under 5%: the router does not slow requests at this many routes.")


if __name__ == "__main__":
    main()
