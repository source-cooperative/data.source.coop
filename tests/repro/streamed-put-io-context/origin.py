"""Fake backend for the reproducer: a body sink plus a deliberately slow endpoint.

`/sink` swallows a streamed PUT and returns 200 — it stands in for S3.
`/slow` sleeps, standing in for the Source API lookup and the STS exchange that
the gateway awaits between capturing the request body and forwarding it. The
sleep is what keeps several requests parked on an await at once, which is the
condition under which the shared wasm-bindgen executor can poll one request's
future while workerd's current I/O context belongs to another.

Threaded on purpose: a single-threaded server would serialize the proxy's
requests and destroy the concurrency the bug needs.
"""

import os
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(os.environ.get("REPRO_ORIGIN_PORT", "9101"))
SLOW_SECONDS = float(os.environ.get("REPRO_SLOW_SECONDS", "0.25"))


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_GET(self):
        if self.path.startswith("/slow"):
            time.sleep(SLOW_SECONDS)
            return self._send(200, b"slow-done")
        return self._send(404, b"")

    def do_PUT(self):
        # Drain the body so the client side of the pipe completes normally.
        remaining = int(self.headers.get("content-length") or 0)
        if remaining:
            while remaining > 0:
                chunk = self.rfile.read(min(remaining, 1 << 20))
                if not chunk:
                    break
                remaining -= len(chunk)
        else:
            # Chunked / unknown length: read to EOF of this message.
            self.rfile.read()
        return self._send(200, b"sunk")

    def _send(self, status, body):
        self.send_response(status)
        self.send_header("content-type", "text/plain")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass  # keep the repro output readable


if __name__ == "__main__":
    print(f"origin on :{PORT} (slow={SLOW_SECONDS}s)", flush=True)
    try:
        ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
    except KeyboardInterrupt:
        sys.exit(0)
