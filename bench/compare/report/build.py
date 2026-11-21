#!/usr/bin/env python3
"""Fill the report template with bench/compare/results.json: python3 bench/compare/report/build.py [results.json]"""

import json
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
COMPARE = HERE.parent


def versions():
    out = {}
    for name in ["express", "fastify", "ws"]:
        out[name] = json.loads((COMPARE / "node" / "node_modules" / name / "package.json").read_text())["version"]
    script = ("import importlib.metadata as m, json; print(json.dumps({p: m.version(p) for p in "
              "['fastapi', 'uvicorn', 'flask', 'gunicorn', 'websockets']}))")
    out.update(json.loads(subprocess.run([str(COMPARE / "python" / ".venv" / "bin" / "python"), "-I", "-c", script],
                                         capture_output=True, text=True, check=True).stdout))
    for line in (COMPARE / "go" / "go.mod").read_text().splitlines():
        if "gorilla/websocket" in line:
            out["gorilla"] = line.split()[-1]
    return out


def main():
    results = Path(sys.argv[1]) if len(sys.argv) > 1 else COMPARE / "results.json"
    page = (HERE / "template.html").read_text()
    # "</" inside a JSON string must not close the script element.
    data = results.read_text().replace("</", "<\\/")
    page = page.replace("__DATA__", data).replace("__VERSIONS__", json.dumps(versions()))
    (HERE / "index.html").write_text(page)
    print(f"wrote {HERE / 'index.html'}")


if __name__ == "__main__":
    main()
