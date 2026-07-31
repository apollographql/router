import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get("content-length", "0"))
        self.rfile.read(length)
        languages = [
            self.headers.get("x-wasm-rust", "missing"),
            self.headers.get("x-wasm-node", "missing"),
            self.headers.get("x-wasm-python", "missing"),
            self.headers.get("x-wasm-go", "missing"),
            self.headers.get("x-wasm-java", "missing"),
            self.headers.get("x-wasm-scala", "missing"),
        ]
        body = json.dumps({"data": {"me": ",".join(languages)}}).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, format, *args):
        pass


ThreadingHTTPServer(("127.0.0.1", 4005), Handler).serve_forever()
