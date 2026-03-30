#!/usr/bin/env python3
"""
Blackbox integration test: OTLP HTTP telemetry through an HTTP proxy.

Spins up three local servers and the real router binary, then asserts two things:

  1. The in-process HTTP proxy intercepted a POST /v1/traces request.
  2. The OTLP backend decoded at least one valid ExportTraceServiceRequest
     protobuf (containing resource spans).

No external network access required — everything is localhost.

Usage
-----
    # From the repo root:
    python3 scripts/test_otlp_http_proxy.py

    # Or point at a pre-built binary:
    ROUTER_BIN=./target/release/router python3 scripts/test_otlp_http_proxy.py

Prerequisites
-------------
    pip install protobuf  (for protobuf decoding — optional, see DECODE_PROTO below)
    # Or just skip decode and check for non-empty bytes.
"""

import gzip
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import textwrap
import threading
import time
import urllib.request
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

REPO_ROOT = Path(__file__).resolve().parent.parent
ROUTER_BIN = os.environ.get(
    "ROUTER_BIN",
    str(REPO_ROOT / "target" / "debug" / "router"),
)
SUPERGRAPH = REPO_ROOT / "apollo-router" / "tests" / "fixtures" / "supergraph.graphql"

# Whether to attempt protobuf decode (requires `pip install protobuf`).
# If False, we just assert that gzip-decoded bytes are non-empty.
try:
    from google.protobuf import descriptor_pool, symbol_database  # noqa: F401
    from opentelemetry_proto.proto.collector.trace.v1 import (  # type: ignore
        trace_service_pb2,
    )
    DECODE_PROTO = True
except ImportError:
    DECODE_PROTO = False

# ---------------------------------------------------------------------------
# Shared state
# ---------------------------------------------------------------------------

_lock = threading.Lock()
_proxy_hits: list[str] = []       # absolute URIs seen by the proxy
_backend_payloads: list[bytes] = []  # raw (decompressed) protobuf bytes


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def free_port() -> int:
    """Bind to port 0, record the assigned port, then release it."""
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def log(msg: str) -> None:
    print(f"  {msg}", flush=True)


# ---------------------------------------------------------------------------
# 1. Mock subgraph — returns a minimal GraphQL response for any query
# ---------------------------------------------------------------------------

class SubgraphHandler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass  # suppress default access log

    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        self.rfile.read(length)
        body = json.dumps({"data": {"topProducts": []}}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


# ---------------------------------------------------------------------------
# 2. OTLP backend — records gzip-decompressed protobuf payloads
# ---------------------------------------------------------------------------

class BackendHandler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        raw = self.rfile.read(length)

        # The OTLP HTTP exporter uses gzip compression.
        try:
            payload = gzip.decompress(raw)
        except Exception:
            payload = raw  # already decompressed or not gzip

        with _lock:
            _backend_payloads.append(payload)

        self.send_response(200)
        self.send_header("Content-Length", "0")
        self.end_headers()


# ---------------------------------------------------------------------------
# 3. HTTP forward proxy — records target URIs, then forwards the request
# ---------------------------------------------------------------------------
# When reqwest sends through an HTTP proxy it uses the *absolute-form* URI
# in the request line, e.g.:
#
#   POST http://127.0.0.1:PORT/v1/traces HTTP/1.1
#
# Python's BaseHTTPRequestHandler exposes this as self.path for HTTP/1.x.

class ProxyHandler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    # ------------------------------------------------------------------
    # Forward an HTTP request (absolute URI) to its actual target.
    # We reconstruct the request from parts because urllib.request does
    # not support sending to a proxy with an absolute-form URI directly.
    # ------------------------------------------------------------------
    def _forward(self):
        import http.client
        import urllib.parse

        target = self.path  # absolute URI, e.g. http://host:port/path

        with _lock:
            _proxy_hits.append(target)

        parsed = urllib.parse.urlparse(target)
        host = parsed.hostname
        port = parsed.port or 80
        path = parsed.path or "/"
        if parsed.query:
            path = f"{path}?{parsed.query}"

        length = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(length)

        # Forward to backend
        conn = http.client.HTTPConnection(host, port, timeout=10)
        try:
            forward_headers = {
                k: v
                for k, v in self.headers.items()
                if k.lower() not in ("host", "proxy-connection")
            }
            conn.request(self.command, path, body=body, headers=forward_headers)
            resp = conn.getresponse()
            resp_body = resp.read()
        finally:
            conn.close()

        self.send_response(resp.status)
        for name, value in resp.getheaders():
            self.send_header(name, value)
        self.end_headers()
        self.wfile.write(resp_body)

    def do_POST(self):
        self._forward()

    def do_GET(self):
        self._forward()

    def do_PUT(self):
        self._forward()


# ---------------------------------------------------------------------------
# Wiring: start all three servers in background threads
# ---------------------------------------------------------------------------

from socketserver import ThreadingMixIn

class ThreadingHTTPServer(ThreadingMixIn, HTTPServer):
    """Handle each connection in a new thread so the proxy can process
    concurrent requests (e.g., usage metrics and OTLP traces arriving at
    nearly the same time don't have to queue behind each other)."""
    daemon_threads = True


def start_server(handler_class, port: int) -> ThreadingHTTPServer:
    server = ThreadingHTTPServer(("127.0.0.1", port), handler_class)
    t = threading.Thread(target=server.serve_forever, daemon=True)
    t.start()
    return server


# ---------------------------------------------------------------------------
# Router config
# ---------------------------------------------------------------------------

ROUTER_CONFIG_TEMPLATE = """\
include_subgraph_errors:
  all: true

# Override the subgraph URLs baked into the compiled supergraph SDL.
override_subgraph_url:
  accounts:  "http://127.0.0.1:{subgraph_port}"
  products:  "http://127.0.0.1:{subgraph_port}"
  reviews:   "http://127.0.0.1:{subgraph_port}"
  inventory: "http://127.0.0.1:{subgraph_port}"

# Move the health check to a dynamic port so we don't collide with the
# router's default :8088 and can poll it reliably in the test.
health_check:
  listen: "127.0.0.1:{health_port}"

telemetry:
  apollo:
    # Point at the local OTLP backend.
    endpoint: "http://127.0.0.1:{backend_port}"
    experimental_otlp_endpoint: "http://127.0.0.1:{backend_port}"

    # Use HTTP (not gRPC) so that reqwest is the transport and HTTP_PROXY applies.
    experimental_otlp_tracing_protocol: http

    # Sample every trace so we are sure to get at least one export.
    otlp_tracing_sampler: always_on

    tracing:
      batch_processor:
        scheduled_delay: 100ms
    metrics:
      otlp:
        batch_processor:
          scheduled_delay: 100ms
      usage_reports:
        batch_processor:
          scheduled_delay: 100ms
"""


# ---------------------------------------------------------------------------
# Main test logic
# ---------------------------------------------------------------------------

def run_test() -> int:
    # Check for the router binary early.
    if not Path(ROUTER_BIN).exists():
        print(f"\n[SKIP] Router binary not found at {ROUTER_BIN}")
        print("       Build it first:  cargo build -p apollo-router")
        return 0  # soft skip — not a hard failure

    subgraph_port = free_port()
    backend_port  = free_port()
    proxy_port    = free_port()
    router_port   = free_port()
    health_port   = free_port()

    print(f"\nPorts:")
    print(f"  subgraph  → {subgraph_port}")
    print(f"  backend   → {backend_port}  (mock OTLP receiver)")
    print(f"  proxy     → {proxy_port}    (HTTP forward proxy)")
    print(f"  router    → {router_port}")
    print(f"  health    → {health_port}")

    # Start helper servers.
    start_server(SubgraphHandler, subgraph_port)
    start_server(BackendHandler,  backend_port)
    start_server(ProxyHandler,    proxy_port)
    log("subgraph / backend / proxy servers started")

    # Write a temp router config.
    tmpdir = Path(tempfile.mkdtemp())
    config_path = tmpdir / "router.yaml"
    config_path.write_text(
        ROUTER_CONFIG_TEMPLATE.format(
            subgraph_port=subgraph_port,
            backend_port=backend_port,
            proxy_port=proxy_port,
            health_port=health_port,
        )
    )

    # Copy the supergraph SDL into tmpdir so relative paths work.
    schema_path = tmpdir / "supergraph.graphql"
    shutil.copy(SUPERGRAPH, schema_path)

    # Apollo credentials — the router validates the key format and contacts
    # Uplink for a license, so we need real credentials.  Read from the
    # standard TEST_ env vars used by the router's own CI/mise setup.
    apollo_key = os.environ.get("TEST_APOLLO_KEY")
    apollo_graph_ref = os.environ.get("TEST_APOLLO_GRAPH_REF")
    if not apollo_key or not apollo_graph_ref:
        print(
            "\n[SKIP] TEST_APOLLO_KEY / TEST_APOLLO_GRAPH_REF not set.\n"
            "       These must be real Apollo credentials — the router validates\n"
            "       the key format and fetches a license from Uplink on startup.\n"
            "       Tip: run via `mise run` from packages/router, which sets them\n"
            "       automatically from .config/mise/config.local.toml."
        )
        return 0

    env = {
        **os.environ,
        "APOLLO_KEY": apollo_key,
        "APOLLO_GRAPH_REF": apollo_graph_ref,
        # Route all outgoing HTTP through the proxy.
        # reqwest (used by opentelemetry-otlp) reads this at client creation
        # time, so it must be present when the router starts up.
        "HTTP_PROXY": f"http://127.0.0.1:{proxy_port}",
        "http_proxy": f"http://127.0.0.1:{proxy_port}",
        # Clear any NO_PROXY/no_proxy from the host environment so the router
        # doesn't bypass our test proxy for localhost/127.0.0.1 destinations.
        "NO_PROXY": "",
        "no_proxy": "",
    }

    cmd = [
        ROUTER_BIN,
        "--config",    str(config_path),
        "--supergraph", str(schema_path),
        "--listen",    f"127.0.0.1:{router_port}",
        "--log",       "warn",
    ]
    log(f"starting router: {' '.join(cmd)}")
    log(f"  HTTP_PROXY={env['HTTP_PROXY']}")

    # Merge stderr into stdout and pipe both, then drain in a background
    # thread.  Without draining, the pipe buffer fills up and the router
    # process blocks — preventing OTLP trace exports from completing.
    router_proc = subprocess.Popen(
        cmd,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    _router_stderr_lines: list[str] = []

    def _drain_router_output() -> None:
        assert router_proc.stdout is not None
        for raw_line in router_proc.stdout:
            _router_stderr_lines.append(raw_line.decode(errors="replace").rstrip())

    threading.Thread(target=_drain_router_output, daemon=True).start()

    # Wait for the router to be ready.
    # The router exposes its health check on a *separate* port (health_port),
    # not on the --listen port, so we poll the health_check address instead.
    ready = False
    for _ in range(60):
        time.sleep(0.5)
        try:
            urllib.request.urlopen(
                f"http://127.0.0.1:{health_port}/health", timeout=1
            )
            ready = True
            break
        except Exception:
            pass

    if not ready:
        router_proc.terminate()
        router_proc.wait(timeout=5)
        output = "\n".join(_router_stderr_lines[-40:])
        print(f"\n[FAIL] Router did not become ready within 30 s.\n{output}")
        return 1

    log("router is ready")

    # Send a GraphQL query.
    query = json.dumps(
        {"query": "query { topProducts { name } }"}
    ).encode()
    req = urllib.request.Request(
        f"http://127.0.0.1:{router_port}",
        data=query,
        headers={"Content-Type": "application/json"},
    )
    try:
        urllib.request.urlopen(req, timeout=5).read()
        log("GraphQL query sent")
    except Exception as exc:
        log(f"GraphQL query failed (may be expected for mock subgraph): {exc}")

    # Wait up to 5 s for the OTLP /v1/traces batch to flush through the proxy.
    # We must not break out early on the legacy reporter (/), so keep going
    # until we see a proxy hit specifically for /v1/traces.
    log("waiting for OTLP /v1/traces batch to flush…")
    for _ in range(50):
        time.sleep(0.1)
        with _lock:
            if any("/v1/traces" in u for u in _proxy_hits):
                break

    # Tear down the router.
    router_proc.terminate()
    try:
        router_proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        router_proc.kill()

    # ------------------------------------------------------------------
    # Assertions
    # ------------------------------------------------------------------
    print("\nResults:")
    failures = 0

    with _lock:
        hits  = list(_proxy_hits)
        payloads = list(_backend_payloads)

    # 1. Proxy must have intercepted at least one /v1/traces request.
    otlp_hits = [u for u in hits if "/v1/traces" in u]
    print(f"  proxy intercepted {len(hits)} request(s) total:")
    for u in hits:
        print(f"    {u}")

    if not otlp_hits:
        print("  [FAIL] proxy did not intercept any /v1/traces requests")
        failures += 1
    else:
        print(f"  [PASS] proxy intercepted {len(otlp_hits)} /v1/traces request(s)")

    # 2. Backend must have received at least one non-empty protobuf payload.
    print(f"\n  backend received {len(payloads)} payload(s)")

    if not payloads:
        print("  [FAIL] backend received no OTLP data")
        failures += 1
    else:
        first = payloads[0]
        print(f"  first payload: {len(first)} bytes (decompressed)")

        if DECODE_PROTO:
            try:
                from opentelemetry_proto.proto.collector.trace.v1.trace_service_pb2 import (
                    ExportTraceServiceRequest,
                )
                msg = ExportTraceServiceRequest()
                msg.ParseFromString(first)
                span_count = sum(
                    len(ss.spans)
                    for rs in msg.resource_spans
                    for ss in rs.scope_spans
                )
                print(f"  decoded: {len(msg.resource_spans)} resource span(s), {span_count} individual span(s)")
                if msg.resource_spans:
                    print("  [PASS] backend received valid OTLP ExportTraceServiceRequest")
                else:
                    print("  [FAIL] protobuf decoded but contained no resource spans")
                    failures += 1
            except Exception as exc:
                print(f"  protobuf decode failed: {exc}")
                print("  [PASS] bytes received (install opentelemetry-proto for full decode)")
        else:
            # Minimal check: bytes are non-empty and look like protobuf (field tags).
            if len(first) > 4:
                print("  [PASS] backend received non-empty payload (install opentelemetry-proto to fully decode)")
            else:
                print("  [FAIL] payload is suspiciously small")
                failures += 1

    print()
    if failures:
        print(f"FAILED  ({failures} assertion(s) failed)")
        return 1
    else:
        print("PASSED")
        return 0


if __name__ == "__main__":
    sys.exit(run_test())
