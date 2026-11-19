# The HTTP benchmark server in Python: the standard library's ThreadingHTTPServer with HTTP/1.1
# keep-alive. (A framework such as FastAPI under uvicorn is not installed on the benchmark
# machine; see bench/README.md.)
import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    disable_nagle_algorithm = True

    def send(self, body: bytes, content_type: str):
        self.send_response(200)
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
        else:
            self.send_error(404)

    def log_message(self, *args):
        pass


ThreadingHTTPServer.daemon_threads = True
server = ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), Handler)
print(f"listening on {sys.argv[1]}", file=sys.stderr, flush=True)
server.serve_forever()
