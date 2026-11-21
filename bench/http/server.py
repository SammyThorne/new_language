# The HTTP benchmark server in Python: the standard library's ThreadingHTTPServer with HTTP/1.1
# keep-alive, with the same endpoints as server.vi. (A framework such as FastAPI under uvicorn is
# not installed on the benchmark machine; see bench/README.md.)
import json
import re
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

OTHER = [re.compile(f"^/api/v1/resource{i}/([^/]+)$") for i in range(10)]
USER = re.compile("^/users/([^/]+)$")


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    disable_nagle_algorithm = True

    def send(self, body: bytes, content_type: str, status: int = 200):
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/plaintext":
            self.send(b"Hello, World!", "text/plain; charset=utf-8")
        elif self.path == "/json":
            self.send(json.dumps({"message": "Hello, World!"}).encode(), "application/json")
        elif self.path == "/items":
            items = [{"id": i, "name": f"item {i}", "price": i * 1.5, "tags": ["a", "b"]} for i in range(20)]
            self.send(json.dumps(items).encode(), "application/json")
        elif any(pattern.match(self.path) for pattern in OTHER):
            self.send(b"other", "text/plain")
        elif m := USER.match(self.path):
            try:
                id = int(m.group(1))
            except ValueError:
                self.send(b"id must be an integer", "text/plain", 400)
                return
            user = {"id": id, "name": f"user {id}", "email": f"user{id}@example.com", "active": id % 2 == 0}
            self.send(json.dumps(user).encode(), "application/json")
        else:
            self.send_error(404)

    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
        if self.path != "/orders":
            self.send_error(404)
            return
        try:
            order = json.loads(body)
            total = 0.0
            count = 0
            for line in order["lines"]:
                total += line["qty"] * line["price"]
                count += line["qty"]
            receipt = {"customer": order["customer"], "count": count, "total": total}
        except (ValueError, KeyError, TypeError) as error:
            self.send(str(error).encode(), "text/plain", 400)
            return
        self.send(json.dumps(receipt).encode(), "application/json")

    def log_message(self, *args):
        pass


ThreadingHTTPServer.daemon_threads = True
server = ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), Handler)
print(f"listening on {sys.argv[1]}", file=sys.stderr, flush=True)
server.serve_forever()
