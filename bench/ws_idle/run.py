"""Hold many idle WebSocket connections open and measure the server's memory for each.

Usage: python3 bench/ws_idle/run.py [--connections N]   (default 50000)

Builds `server.wb` (release) and `client.go`, starts the server, records its resident memory,
opens the connections, waits for the server to settle, records its memory again, then checks
that every connection still answers. Prints memory per connection.
"""

import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
COMPILER = ROOT / "target" / "release" / "weby"


def rss_kib(pid: int) -> int:
    for line in Path(f"/proc/{pid}/status").read_text().splitlines():
        if line.startswith("VmRSS:"):
            return int(line.split()[1])
    return 0


def main() -> None:
    count = int(sys.argv[sys.argv.index("--connections") + 1]) if "--connections" in sys.argv else 50000
    go = shutil.which("go") or str(Path.home() / ".local" / "go" / "bin" / "go")
    with tempfile.TemporaryDirectory() as out:
        out = Path(out)
        subprocess.run([COMPILER, "build", "--release", HERE / "server.wb", "-o", out / "server"], check=True)
        subprocess.run([go, "build", "-o", out / "client", HERE / "client.go"], check=True, env={"GOTOOLCHAIN": "local", **__import__("os").environ})
        server = subprocess.Popen([out / "server"], stderr=subprocess.PIPE, stdout=subprocess.DEVNULL, text=True)
        port = int(server.stderr.readline().split()[-1])
        time.sleep(0.5)
        base = rss_kib(server.pid)
        client = subprocess.Popen([out / "client", str(port), str(count)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        started = time.time()
        connected = client.stdout.readline().strip()
        took = time.time() - started
        time.sleep(2)
        loaded = rss_kib(server.pid)
        client.stdin.write("\n")
        client.stdin.flush()
        echoed = client.stdout.readline().strip()
        client.wait()
        server.terminate()
        server.wait()
        opened = int(connected.split()[1])
        print(f"{connected} in {took:.1f} s; {echoed}")
        print(f"server memory: {base} KiB idle, {loaded} KiB with {opened} connections")
        print(f"per connection: {(loaded - base) / max(opened, 1):.1f} KiB")


if __name__ == "__main__":
    main()
